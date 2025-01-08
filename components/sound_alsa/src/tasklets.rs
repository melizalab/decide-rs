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
use sndfile;
use serde::Deserialize;
use sndfile::{ReadOptions, SndFileIO};
use walkdir::WalkDir;
use decide_protocol::error::DecideError;
use thiserror::Error;

pub fn import_audio(switch: Arc<AtomicU32>,
                queue: Arc<Mutex<HashMap<OsString, (Vec<i16>,u32)>>>,
                channels: bool,
                conf_path: String,
                audio_count: Arc<AtomicU32>) {
    thread::spawn(move || {
        tracing::info!("sound-alsa importing audio from config file {:?}", conf_path);
        let config_file_path = Path::new(&conf_path);
        let config_file = File::open(config_file_path)
            .map_err(|_e| DecideError::Component {source:
                PlaybackError::FileAccessError {file: conf_path.clone()}.into()
            }).unwrap();
        let exp_config: ConfFile = serde_json::from_reader(config_file)
            .map_err(|e| DecideError::Component {source: e.into()}).unwrap();
            // important to retain the error of parsing process

        tracing::info!("sound-alsa stimuli folder specified by config file: {:?}", &exp_config.stimulus_root);
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
                    .map_err(|_e| DecideError::Component {source:
                        PlaybackError::PlaybackMemoryError {tag: "stimulus queue lock".to_string()}.into()
                    }).unwrap(); // this will block if playback is underway
                //avoid duplicates
                if !stim_queue.contains_key(&fname) {
                    //make sure file is an audio file with "wav" extension
                    if path.extension().is_some_and(|ext| ext == "wav") {
                        let mut audio_file = sndfile::OpenOptions::ReadOnly(ReadOptions::Auto).from_path(path)
                            .map_err(|_e| DecideError::Component {source:
                                PlaybackError::FileAccessError {file: String::from(path.to_str().unwrap())}.into()
                            }).unwrap();
                        let wav: Vec<i16> = audio_file.read_all_to_vec()
                            .map_err(|_e| DecideError::Component {source:
                                PlaybackError::StimParseError {file: fname.clone() }.into()
                            }).unwrap();
                        let wav_channels = audio_file.get_channels();
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
        tracing::info!("sound-alsa import completed");
        //add number of stims:
        let length = queue.lock()
            .map_err(|_e| DecideError::Component {source:
                PlaybackError::PlaybackMemoryError {tag: "stimulus queue lock".to_string()}.into()
            }).unwrap().keys().len() as u32;
        audio_count.store(length, Ordering::Relaxed);
        //ref line 117
        switch.store(0, Ordering::Release);
        let sw = switch.as_ref();
        wake_all(sw);
    }).join().expect("Sound-Alsa: Import Failed!");
}

pub fn process_audio(wav: Vec<i16>, wav_channels: usize, hw_channels: u32) -> (Vec<i16>,u32){
    let mut result = Vec::new();
    if wav_channels == 1 {
        result = wav;
        if hw_channels == 2 {
            result = result.into_iter()
                .map(|note| [note, note])
                .flatten()
                .map(|f| f )
                .collect::<Vec<i16>>()
        }
    } else if wav_channels == 2 {
        result = wav;
        if hw_channels == 1 {
            result = result.into_iter()
                .enumerate()
                .filter(|f| f.0.clone() % 2 == 0)
                .map(|f| f.1)
                .collect::<Vec<_>>()
        }
    };
    let length = result.len() as u32;
    (result, length)
}

pub fn get_hw_config<'a>(pcm: &'a PCM, config: &'a Config) -> Result<bool, String>{

    let hwp = HwParams::any(&pcm)
        .map_err(|_e| DecideError::Component {source:
            PlaybackError::HardwareSetupError {tag: "hardware params initialization".to_string()}.into()
        }).unwrap();
    hwp.set_channels(config.channels.clone())
        .map_err(|_e| DecideError::Component {source:
            PlaybackError::HardwareSetupError {tag: "playback channel count".to_string()}.into()
        }).unwrap();
    hwp.set_rate(config.sample_rate, alsa::ValueOr::Nearest)
        .map_err(|_e| DecideError::Component {source:
            PlaybackError::HardwareSetupError {tag: "sampling rate".to_string()}.into()
        }).unwrap();
    hwp.set_access(Access::RWInterleaved)
        .map_err(|_e| DecideError::Component {source:
            PlaybackError::HardwareSetupError {tag: "frame reading mode".to_string()}.into()
        }).unwrap();
    hwp.set_format(Format::s16())
        .map_err(|_e| DecideError::Component {source:
            PlaybackError::HardwareSetupError {tag: "playback bitrate".to_string()}.into()
        }).unwrap();
    hwp.set_buffer_size(1024)
        .map_err(|_e| DecideError::Component {source:
            PlaybackError::HardwareSetupError {tag: "buffer size".to_string()}.into()
        }).unwrap();
    // hwp.set_period_size(512, alsa::ValueOr::Nearest).unwrap();
    pcm.hw_params(&hwp)
        .map_err(|_e| DecideError::Component {source:
            PlaybackError::HardwareSetupError {tag: "setting hardware parameters".to_string()}.into()
        }).unwrap();
    Ok(true)
}

pub fn playback_io(pcm: &PCM, io: &mut alsa::pcm::IO<i16>, data: &Vec<i16>, frames: u32, playback: &Arc<AtomicU32>)
               -> Result<bool, String> {
    let frames: usize = frames.try_into().unwrap();
    let _avail = match pcm.avail_update() {
        Ok(n) => n,
        Err(e) => {
            tracing::warn!("sound-alsa failed to call available update, recovering from {}", e);
            pcm.recover(e.errno() as std::os::raw::c_int, true)
                .map_err(|_e| DecideError::Component {source:
                    PlaybackError::PlaybackError {tag: "recover".to_string()}.into()
                }).unwrap();
            pcm.avail_update()
                .map_err(|_e| DecideError::Component {source:
                    PlaybackError::PlaybackError {tag: "get available buffer".to_string()}.into()
                }).unwrap()
        }
    } as usize;
    // tracing::debug!("Available buffer for playback is {:?}", avail);
    let mut pointer = 0;
    let mut _written: usize = 0;
    //loop while playing
    while (pointer < frames-1) & (playback.load(Ordering::Acquire) == 1) {
        let slice = if pointer+512>frames {&data[pointer..]} else {&data[pointer..pointer+512]};
        _written = match io.writei(slice) {
            Ok(n) => n,
            Err(e) => {
                tracing::error!("Recovering from {}", e);
                pcm.recover(e.errno() as std::os::raw::c_int, true)
                    .map_err(|_e| DecideError::Component {source:
                        PlaybackError::PlaybackError {tag: "recover".to_string()}.into()
                    }).unwrap();
                0
            }
        };
        pointer += _written;
        match pcm.state() {
            State::Running => {
            }, // All fine
            State::Prepared => {
                pcm.start()
                    .map_err(|_e| DecideError::Component {source:
                        PlaybackError::PlaybackError {tag: "start".to_string()}.into()
                    }).unwrap();

            },
            State::XRun => {
                tracing::warn!("underrun in audio output stream!, will call prepare()");
                pcm.prepare()
                    .map_err(|_e| DecideError::Component {source:
                        PlaybackError::PlaybackError {tag: "prepare".to_string()}.into()
                    }).unwrap();
            },
            State::Suspended => {
                tracing::error!("sound-alsa suspended, will call prepare()");
                pcm.prepare()
                    .map_err(|_e| DecideError::Component {source:
                        PlaybackError::PlaybackError {tag: "prepare".to_string()}.into()
                    }).unwrap();
            },
            n @ _ => panic!("sound-alsa unexpected pcm state {:?}", n),
        };
    };
    Ok(true)
}

#[derive(Deserialize)]
pub struct Config {
    pub audio_device: String,
    pub card: String,
    pub volume: u32,
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

#[derive(Error, Debug)]
pub enum PlaybackError {
    #[error("could not send state update")]
    SendError,
    #[error("error trying to open file {file:?}")]
    FileAccessError{file: String},
    #[error("error trying to read wav file {file:?}")]
    StimParseError{file: OsString},
    #[error("error trying to set up {tag:?} for playback hardware. Check physical connections.")]
    HardwareSetupError{tag: String},
    #[error("error trying to set up {tag:?} for playback software. Try rebooting beaglebone.")]
    SoftwareSetupError{tag: String},
    #[error("stimulus id {name:?} does not exist in queue hashmap. Check stimulus directory.")]
    StimulusMissing{name: String},
    #[error("error trying to {tag:?} playback. Try restarting decide-core or rebooting.")]
    PlaybackError{tag: String},
    #[error("error accessing playback variable: {tag:?}. Please document and report conditions.")]
    PlaybackMemoryError{tag: String},
}