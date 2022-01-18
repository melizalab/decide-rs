use std::collections::HashMap;
use std::ffi::OsString;
use std::fs::read_dir;
use std::path::Path;
use async_trait::async_trait;
use decide_proto::{Component,
                   error::{ComponentError, DecideError}
};
use prost::Message;
use prost_types::Any;
use serde::Deserialize;
use std::sync::{atomic::{AtomicBool, AtomicU8, Ordering}, Arc, Mutex};
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering::Acquire;
use std::thread;
use tokio::{self, sync::mpsc};
use tokio::sync::mpsc::Sender;

use alsa::{Direction, ValueOr,
            pcm::{PCM, HwParams, Format, Access, State}};
use audrey::read::BufFileReader;
use wav::{bit_depth, BitDepth};
use decide_proto::error::ComponentError::{FileAccessError, PCM_HwConfigErr, PCM_InitErr};

pub mod sound_alsa {
    include!(concat!(env!("OUT_DIR"), "/_.rs"));
}
#[derive(Deserialize)]
pub struct Config {
    audio_device: String,
    sample_rate: u32,
    //buffer_size: u32,
    channel: u32,
}
pub struct AlsaPlayback {
    //mode: Arc<Mutex<String>>,
    dir: Arc<Mutex<String>>,
    playlist: Arc<Mutex<Vec<String>>>,
    filename: Arc<Mutex<String>>,
    playback: Arc<Mutex<String>>, //pause or resume
    state_sender: Option<mpsc::Sender<Any>>,
}

#[async_trait]
impl Component for AlsaPlayback {
    type State = sound_alsa::State;
    type Params = sound_alsa::Params;
    type Config = Config;
    const STATE_TYPE_URL: &'static str = "";
    const PARAMS_TYPE_URL: &'static str = "";

    fn new(config: Self::Config) -> Self {
        AlsaPlayback{
            //mode: Arc::new(Mutex::new("".to_string())),
            dir: Arc::new(Mutex::new("".to_string())),
            playlist: Arc::new(Mutex::new(Vec::new())),
            filename: Arc::new(Mutex::new(String::from("None"))),
            playback: Arc::new(Mutex::new(String::from("Stop"))),
            state_sender: None,
        }
    }

    fn init(&mut self, config: Self::Config, state_sender: Sender<Any>) {
        self.state_sender = Some(sender.clone());
        let dir = self.dir.clone();
        let playlist = self.playlist.clone();
        let mut filename = self.filename.clone();
        let mut playback = self.playback.clone();

        thread::spawn(move|| {
            let pcm = PCM::new(&*config.audio_device, Direction::Playback, false)
                .map_err(|e| PCM_InitErr { source: e, dev_name: &config.audio_device }).unwrap();

            let hwp = HwParams::any(&pcm)
                .map_err(|e| PCM_HwConfigErr {source:e, param: "init"}).unwrap();
            hwp.set_channels(config.channel)
                .map_err(|e| PCM_HwConfigErr {source: e, param: "channel"}).unwrap();
            hwp.set_rate(config.sample_rate, ValueOr::Nearest)
                .map_err(|e| PCM_HwConfigErr {source:e, param: "sample_rate"}).unwrap();
            hwp.set_access(Access::RWInterleaved)
                .map_err(|e| PCM_HwConfigErr {source:e, param: "access"}).unwrap();
            hwp.set_format(Format::s16())
                .map_err(|e| PCM_HwConfigErr {source:e, param: "format"}).unwrap();

            let swp = pcm.sw_params_current().unwrap();
            swp.set_start_threshold(hwp.get_buffer_size().unwrap()).unwrap();
            pcm.sw_params(&swp).unwrap();
            let io = pcm.io_i16().unwrap();

            let mut queue: HashMap<OsString, Vec<i16>> = std::collections::HashMap::new();
            for result in read_dir(Path::new(&dir.lock().unwrap())).unwrap() {
                let entry = result.unwrap();
                let path = entry.path();
                let file = entry.file_name();
                if path.extension().unwrap() == "wav" {
                    let mut wav = audrey::open(path).unwrap();
                    let wav_channels = wav.description().channel_count() ;
                    let hw_channels = config.channel.clone();
                    let audio = process_audio(wav, wav_channels, hw_channels);
                    queue.insert(file, audio);
                }
            }
            tracing::trace!("imported audio files");
            //Todo: send init
            'thread: loop {
                let playlist = playlist.lock().unwrap();
                'queue: for stim in playlist.iter() {
                    if stim.ends_with("wav")  {
                        *filename.get_mut().unwrap() = stim.clone();
                        *playback.get_mut().unwrap() = "PLAYING".to_string();
                        io.writei(&*queue[stim.into()]).unwrap();
                        while pcm.state() == State::Running {
                            if playback.get_mut() == "PAUSE".to_string() {
                                pcm.pause(true).unwrap();

                            }
                        }

                    }
                }
            }
        });

    }

    fn change_state(&mut self, state: Self::State) -> decide_proto::Result<()> {
        todo!()
    }

    fn set_parameters(&mut self, params: Self::Params) -> decide_proto::Result<()> {
        todo!()
    }

    fn get_state(&self) -> Self::State {
        todo!()
    }

    fn get_parameters(&self) -> Self::Params {
        todo!()
    }
}

fn process_audio(mut wav: BufFileReader, wav_channels: u32, hw_channels: u32) -> Vec<i16>{
    let mut result = Vec::new();
    if wav_channels == 1 {
        let wav = wav.frames::<[i16;1]>()
            .map(Result::unwrap)
            .map(|file| audrey::dasp_frame::Frame::scale_amp(file, 0.8))
            .map(|note|
                if hw_channels == 2 {[note, note]}
                else if hw_channels == 1 {[note]})
            .collect::<Vec<_>>();
    } else if wav_channels == 2 {
        let wav = wav.frames::<[i16;2]>()
            .map(Result::unwrap)
            .map(|file| audrey::dasp_frame::Frame::scale_amp(file, 0.8))
            .map(|note|
                if hw_channels == 2 {[note]}
                else if hw_channels == 1 {[note[0]]})
            .collect::<Vec<_>>();
    };
    result
}

