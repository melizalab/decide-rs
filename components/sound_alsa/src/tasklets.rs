use std::collections::HashMap;
use std::ffi::OsString;
use std::fs::File;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicU32, Ordering};
use std::thread;
use serde_json;
use alsa::{pcm::{Access, Format, HwParams, PCM, State}};
use atomic_wait::wake_all;
use audrey::read::BufFileReader;
use serde::Deserialize;
use walkdir::WalkDir;
use decide_protocol::error::DecideError;

pub fn import_audio(switch: Arc<AtomicU32>,
                queue: Arc<Mutex<HashMap<OsString, (Vec<i16>,u32)>>>,
                channels: bool,
                conf_path: String,
                audio_count: Arc<AtomicU32>) {
    thread::spawn(move || {
        tracing::info!("Begin Importing Audio from {:?}", conf_path);
        let config_file_path = Path::new(&conf_path);
        let config_file = File::open(config_file_path)
            .map_err(|e| panic!("Couldn't Open Config File Specified {:?}", e)).unwrap();
        let exp_config: ConfFile = serde_json::from_reader(config_file)
            .map_err(|e| DecideError::Component { source: e.into()}).unwrap();
        tracing::info!("Stimulus Root Specified as {:?}", &exp_config.stimulus_root);
        let playlist = exp_config.get_names();
        for entry in WalkDir::new(exp_config.stimulus_root.clone())
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| !e.file_type().is_dir())
            .filter(|e| e.file_name().to_str()
                .map(|s| e.depth() == 0 || !s.starts_with("."))
                .unwrap_or(false))
        {
            let fname = Path::new(&entry.file_name().to_str().unwrap())
                .file_stem().unwrap().to_os_string();
            if playlist.contains(&fname) {
                let path = entry.path();
                let mut stim_queue = queue.lock()
                    .map_err(|_e| tracing::error!("Couldn't acquire lock on playlist"))
                    .unwrap();
                //avoid duplicates
                if !stim_queue.contains_key(&fname) {
                    //make sure file is an audio file with "wav" extension
                    if path.extension().is_some_and(|ext| ext == "wav") {
                        let wav = audrey::open(path)
                            .map_err(|e| DecideError::Component { source: e.into() }).unwrap();
                        let wav_channels = wav.description().channel_count();
                        let hw_channels = if channels { 2 } else { 1 };
                        tracing::info!("Importing file {:?}", fname);
                        let stim = process_audio(wav, wav_channels, hw_channels);
                        stim_queue.insert(OsString::from(fname.clone()), stim);
                    }
                }
            } else {
                continue
            }

        }
        tracing::info!("Finished importing audio files");
        //add number of stims:
        let length = queue.lock().unwrap()
            .keys().len() as u32;
        audio_count.store(length, Ordering::Relaxed);
        //ref line 117
        switch.store(0, Ordering::Release);
        let sw = switch.as_ref();
        wake_all(sw);
    }).join().expect("Sound-Alsa: Import Failed!");
}

pub fn process_audio(mut wav: BufFileReader, wav_channels: u32, hw_channels: u32) -> (Vec<i16>,u32){
    let mut result = Vec::new();
    if wav_channels == 1 {
        result = wav.frames::<[i16;1]>()
            .map(Result::unwrap)
            .map(|file| audrey::dasp_frame::Frame::scale_amp(file, 1.0))
            .map(|note| note[0])
            .collect::<Vec<i16>>();
        if hw_channels == 2 {
            result = result.iter()
                .map(|note| [note, note])
                .flatten()
                .map(|f| *f )
                .collect::<Vec<_>>()
        }
    } else if wav_channels == 2 {
        result = wav.frames::<[i16;2]>()
            .map(Result::unwrap)
            .map(|file| audrey::dasp_frame::Frame::scale_amp(file, 1.0))
            .flatten()
            .collect::<Vec<i16>>();
        if hw_channels == 1 {
            result = result.iter()
                .enumerate()
                .filter(|f| f.0.clone() % 2 == 0)
                .map(|f| *f.1)
                .collect::<Vec<_>>()
        }
    };
    let length = result.len() as u32;
    return (result, length);
}

pub fn get_hw_config<'a>(pcm: &'a PCM, config: &'a Config) -> std::result::Result<bool, String>{

    let hwp = HwParams::any(&pcm).unwrap();
    hwp.set_channels(config.channels.clone()).unwrap();
    hwp.set_rate(config.sample_rate, alsa::ValueOr::Nearest).unwrap();
    hwp.set_access(Access::RWInterleaved).unwrap();
    hwp.set_format(Format::s16()).unwrap();
    hwp.set_buffer_size(1024).unwrap(); // A few ms latency by default
    // hwp.set_period_size(512, alsa::ValueOr::Nearest).unwrap();
    pcm.hw_params(&hwp).unwrap();
    Ok(true)
}

pub fn playback_io(pcm: &alsa::PCM, io: &mut alsa::pcm::IO<i16>, data: &Vec<i16>, frames: u32, playback: &Arc<AtomicU32>)
               -> std::result::Result<bool, String> {
    let frames: usize = frames.try_into().unwrap();
    let avail = match pcm.avail_update() {
        Ok(n) => n,
        Err(e) => {
            tracing::warn!("Audio-playback failed to call available update, recovering from {}", e);
            pcm.recover(e.errno() as std::os::raw::c_int, true).unwrap();
            pcm.avail_update().unwrap()
        }
    } as usize;
    tracing::debug!("Available buffer for playback is {:?}", avail);
    let mut pointer = 0;
    let mut _written: usize = 0;
    //loop while playing
    while (pointer < frames-1) & (playback.load(Ordering::Acquire) == 1) {
        let slice = if pointer+512>frames {&data[pointer..]} else {&data[pointer..pointer+512]};
        _written = match io.writei(slice) {
            Ok(n) => n,
            Err(e) => {
                tracing::error!("Recovering from {}", e);
                pcm.recover(e.errno() as std::os::raw::c_int, true).unwrap();
                0
            }
        };
        pointer += _written;
        match pcm.state() {
            State::Running => {
            }, // All fine
            State::Prepared => {
                pcm.start().unwrap();
            },
            State::XRun => {
                tracing::warn!("Underrun in audio output stream!, will call prepare()");
                pcm.prepare().unwrap();
            },
            State::Suspended => {
                tracing::error!("Suspended, will call prepare()");
                pcm.prepare().unwrap();
            },
            n @ _ => panic!("Unexpected pcm state {:?}", n),
        };
    };
    Ok(true)
}

#[derive(Deserialize)]
pub struct Config {
    pub audio_device: String,
    pub sample_rate: u32,
    pub channels: u32,
}

#[derive(Debug, Deserialize)]
struct Stimulus {
    name: String,
    //frequency: u32, // ignore responses and categories fpr now
}

#[derive(Debug, Deserialize)]
struct ConfFile {
    stimulus_root: String,
    stimuli: Vec<Stimulus>
}

impl ConfFile {
    fn get_names(&self) -> Vec<OsString> {
        self.stimuli.iter()
            .map(|stim| OsString::from(&stim.name))
            .collect::<Vec<OsString>>()
    }
}