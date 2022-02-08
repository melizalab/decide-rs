use std::collections::HashMap;
use std::ffi::OsString;
use std::fs::read_dir;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use alsa::{Direction, pcm::{Access, Format, HwParams, PCM, State},
           ValueOr};
use async_trait::async_trait;
use audrey::read::BufFileReader;
use prost::Message;
use prost_types::Any;
use serde::Deserialize;
use tokio::{self, sync::mpsc};
use tokio::sync::mpsc::Sender;

use decide_protocol::{Component,
                      error::{ComponentError, DecideError}
};
use futures::executor::block_on;
use tracing::info;
use proto::state::PlayBack;

pub mod proto {
    include!(concat!(env!("OUT_DIR"), "/_.rs"));
}
#[derive(Deserialize)]
pub struct Config {
    audio_device: String,
    sample_rate: u32,
    channel: u32,
}
pub struct AlsaPlayback {
    //mode: Arc<Mutex<String>>,
    dir: Arc<Mutex<String>>,
    audio_id: Arc<Mutex<String>>,
    playback: Arc<Mutex<PlayBack>>, //pause or resume
    elapsed: Arc<Mutex<Option<prost_types::Duration>>>,
    state_sender: mpsc::Sender<Any>,
}

#[async_trait]
impl Component for AlsaPlayback {
    type State = proto::State;
    type Params = proto::Params;
    type Config = Config;
    const STATE_TYPE_URL: &'static str = "";
    const PARAMS_TYPE_URL: &'static str = "";

    fn new(_config: Self::Config, state_sender: Sender<Any>) -> Self {
        AlsaPlayback{
            //mode: Arc::new(Mutex::new("".to_string())),
            dir: Arc::new(Mutex::new("".to_string())),
            audio_id: Arc::new(Mutex::new(String::from("None"))),
            playback: Arc::new(Mutex::new(PlayBack::Stop)),
            elapsed: Arc::new(Mutex::new(Some(prost_types::Duration{
                seconds:0, nanos:0
            }))),
            state_sender,
        }
    }

    async fn init(&mut self, config: Self::Config) {
        let dir = self.dir.clone();
        let audio_id = self.audio_id.clone();
        let playback = self.playback.clone();
        let elapsed = self.elapsed.clone();
        let sender = self.state_sender.clone();

        thread::spawn(move|| {
            let pcm = PCM::new(&*config.audio_device, Direction::Playback, false)
                .map_err(|_e| ComponentError::PcmInitErr {  dev_name: config.audio_device.clone() }).unwrap();

            let hwp = HwParams::any(&pcm)
                .map_err(|_e| ComponentError::PcmHwConfigErr {param: String::from("init")}).unwrap();
            hwp.set_channels(config.channel)
                .map_err(|_e| ComponentError::PcmHwConfigErr { param: String::from("channel")}).unwrap();
            hwp.set_rate(config.sample_rate, ValueOr::Nearest)
                .map_err(|_e| ComponentError::PcmHwConfigErr {param: String::from("sample_rate")}).unwrap();
            hwp.set_access(Access::RWInterleaved)
                .map_err(|_e| ComponentError::PcmHwConfigErr {param: String::from("access")}).unwrap();
            hwp.set_format(Format::s16())
                .map_err(|_e| ComponentError::PcmHwConfigErr {param: String::from("format")}).unwrap();

            let swp = pcm.sw_params_current().unwrap();
            swp.set_start_threshold(hwp.get_buffer_size().unwrap()).unwrap();
            pcm.sw_params(&swp).unwrap();
            let io = pcm.io_i16().unwrap();

            let mut queue: HashMap<OsString, Vec<i16>> = std::collections::HashMap::new();
            let dir = dir.lock().unwrap().clone();
            'import: loop {
                let mut reader = read_dir(Path::new(&dir)).unwrap();
                let mut entry= reader.next();
                if entry.is_some() {
                    while entry.is_some() {
                        let file = entry.unwrap().unwrap();
                        let path = file.path();
                        let fname = file.file_name();
                        if path.extension().unwrap() == "wav" {
                            let wav = audrey::open(path).unwrap();
                            let wav_channels = wav.description().channel_count();
                            let hw_channels = config.channel.clone();
                            let audio = process_audio(wav, wav_channels, hw_channels);
                            queue.insert(fname,audio);
                        }
                        entry = reader.next();
                    }
                    info!("Finished import, found {:?} wav files", queue.len());
                    break 'import
                } else {info!("Playback directory is empty. Awaiting files at {:?}", dir)}
            }

            info!("Begin Playback loop");
            //Todo: send init
            'stim: loop {
                let stim = audio_id.lock().unwrap();
                let p = playback.lock().unwrap().clone();
                if p == PlayBack::Playing {
                    if stim.ends_with("wav")  {
                        let stim = OsString::from(stim.clone());
                        let mut elapsed = elapsed.lock().unwrap();
                        pcm.hw_params(&hwp).unwrap();
                        io.writei(&*queue[&stim]).unwrap();
                        let timer = std::time::Instant::now();
                        'playback: while pcm.state() == State::Running {
                            let p = playback.try_lock().unwrap().clone();
                            match p {
                                PlayBack::Next => {
                                    pcm.drop().unwrap();
                                    let duration = Some(prost_types::Duration::from(timer.elapsed()));
                                    *elapsed = duration.clone();
                                    let mut playback = playback.lock().unwrap();
                                    *playback = PlayBack::Playing;
                                    let state = Self::State {
                                        audio_id: audio_id.lock().unwrap().clone(),
                                        playback: PlayBack::Playing as i32,
                                        elapsed: duration
                                    };
                                    let message = Any {
                                        value: state.encode_to_vec(),
                                        type_url: Self::STATE_TYPE_URL.into(),
                                    };
                                    block_on(sender.send(message)).unwrap_err();
                                    continue 'stim}
                                PlayBack::Stop => {
                                    let duration = Some(prost_types::Duration::from(timer.elapsed()));
                                    *elapsed = duration.clone();
                                    pcm.drop().unwrap();
                                    let state = Self::State {
                                        audio_id: String::from("None"),
                                        playback: PlayBack::Stop as i32,
                                        elapsed: duration
                                    };
                                    let message = Any {
                                        value: state.encode_to_vec(),
                                        type_url: Self::STATE_TYPE_URL.into(),
                                    };
                                    block_on(sender.send(message)).unwrap();
                                    continue 'stim}
                                PlayBack::Playing => {continue 'playback}
                            };
                        }
                    } else {
                        tracing::error!("Playback set to Playing but filename is invalid");
                        thread::sleep(Duration::from_millis(1000)); continue 'stim}
                } else {thread::sleep(Duration::from_millis(1000)); continue}
            }
        });

    }

    fn change_state(&mut self, state: Self::State) -> decide_protocol::Result<()> {
        let mut playback = self.playback.lock().unwrap();
        *playback = PlayBack::from_i32(state.playback).unwrap();
        let mut audio_id = self.audio_id.lock().unwrap();
        *audio_id = state.audio_id.clone();
        let mut elapsed = self.elapsed.lock().unwrap();
        *elapsed = state.elapsed.clone();

        let sender = self.state_sender.clone();
        tokio::spawn(async move {
            sender
                .send(Any {
                    type_url: String::from(Self::STATE_TYPE_URL),
                    value: state.encode_to_vec(),
                })
                .await
                .map_err(|e| DecideError::Component { source: e.into() })
                .unwrap();
            tracing::trace!("state changed");
        });
        Ok(())
    }

    fn set_parameters(&mut self, params: Self::Params) -> decide_protocol::Result<()> {
        let mut dir = self.dir.lock().unwrap();
        *dir = params.dir;
        Ok(())
    }

    fn get_state(&self) -> Self::State {
        Self::State {
            audio_id: self.audio_id.try_lock().unwrap().clone(),
            playback: self.playback.try_lock().unwrap().clone() as i32,
            elapsed: self.elapsed.try_lock().unwrap().clone(),
        }
    }

    fn get_parameters(&self) -> Self::Params {
        Self::Params {
            dir : self.dir.try_lock().unwrap().clone()
        }

    }
}

fn process_audio(mut wav: BufFileReader, wav_channels: u32, hw_channels: u32) -> Vec<i16>{
    let mut result = Vec::new();
    if wav_channels == 1 {
        result = wav.frames::<[i16;1]>()
            .map(Result::unwrap)
            .map(|file| audrey::dasp_frame::Frame::scale_amp(file, 0.8))
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
            .map(|file| audrey::dasp_frame::Frame::scale_amp(file, 0.8))
            .flatten()
            .collect::<Vec<i16>>();
        if hw_channels == 1 {
            result = result.iter()
                .enumerate()
                .filter(|f| f.0 % 2 == 0)
                .map(|f| *f.1)
                .collect::<Vec<_>>()
        }
    };
    result
}
