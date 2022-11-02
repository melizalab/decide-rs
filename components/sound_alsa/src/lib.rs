use std::collections::HashMap;
use std::ffi::OsString;
use std::fs::read_dir;
use std::path::Path;
use std::sync::{Arc, Mutex, mpsc as std_mpsc};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use alsa::{Direction, pcm::{Access, Format, HwParams, PCM, State},
           ValueOr};
use async_trait::async_trait;
use audrey::read::BufFileReader;
use prost::Message;
use prost_types::Any;
use serde::Deserialize;
use tokio::{self, sync::mpsc::{self, Sender}};

use decide_protocol::{Component,
                      error::{DecideError}
};
use futures::executor::block_on;
use proto::state::PlayBack;

pub mod proto {
    include!(concat!(env!("OUT_DIR"), "/_.rs"));
}
#[derive(Deserialize)]
pub struct Config {
    audio_device: String,
    sample_rate: u32,
    channel: u32,
    audio_dir: String
}
pub struct AlsaPlayback {
    audio_id: Arc<Mutex<String>>,
    playback: Arc<Mutex<PlayBack>>, //pause or resume
    elapsed: Arc<Mutex<Option<prost_types::Duration>>>,
    state_sender: mpsc::Sender<Any>,
    shutdown: Option<(thread::JoinHandle<()>,std_mpsc::Sender<bool>)>,
}

#[async_trait]
impl Component for AlsaPlayback {
    type State = proto::State;
    type Params = proto::Params;
    type Config = Config;
    const STATE_TYPE_URL: &'static str = "melizalab.org/proto/sound_alsa_state";
    const PARAMS_TYPE_URL: &'static str = "melizalab.org/proto/sound_alsa_params";

    fn new(_config: Self::Config, state_sender: Sender<Any>) -> Self {
        AlsaPlayback{
            audio_id: Arc::new(Mutex::new(String::from("None"))),
            playback: Arc::new(Mutex::new(PlayBack::Stop)),
            elapsed: Arc::new(Mutex::new(Some(prost_types::Duration{
                seconds:0, nanos:0
            }))),
            state_sender,
            shutdown: None,
        }
    }

    async fn init(&mut self, config: Self::Config) {

        let dir = config.audio_dir;

        let audio_id = self.audio_id.clone();
        let playback = self.playback.clone();
        let elapsed = self.elapsed.clone();
        let sender = self.state_sender.clone();

        let queue: Arc<Mutex<HashMap<OsString, Vec<i16>>>> = Arc::new(Mutex::new(std::collections::HashMap::new()));
        let queue1 = queue.clone();

        //read in audio files
        tokio::spawn(async move {
            let mut reader = read_dir(Path::new(&dir))
                .map_err(|e| DecideError::Component { source: e.into() }).unwrap();
            let mut entry= reader.next();
            //fails if provided dir is empty
            if entry.is_some() {
                //begin reading files
                while entry.is_some() {
                    let file = entry
                        .unwrap()
                        .map_err(|e| DecideError::Component { source: e.into() }).unwrap();
                    //path() returns the full path of the file
                    let path = file.path();
                    //strip the stored dir from full path
                    let fname = OsString::from(path.clone().strip_prefix(&dir)
                        .map_err(|e|DecideError::Component { source: e.into() })
                        .unwrap());

                    let mut stim_queue = queue1.lock()
                        .map_err(|e| println!("Couldn't acquire lock on playlist"))
                        .unwrap();
                    //avoid duplicates
                    if !stim_queue.contains_key(&fname) {
                        //make sure file is an audio file with "wav" extension
                        if path.extension().unwrap() == "wav" {
                            let wav = audrey::open(path)
                                .map_err(|e| DecideError::Component { source: e.into() }).unwrap();
                            let wav_channels = wav.description().channel_count();
                            let hw_channels = config.channel.clone();
                            let audio = process_audio(wav, wav_channels, hw_channels);
                            stim_queue.insert(OsString::from(fname), audio);
                        }
                    }
                    entry = reader.next();
                }
            } else {tracing::info!("Audio Directory is empty!")}
            tracing::info!("Finished importing audio files")
        });

        let (sd_tx, sd_rx) = std_mpsc::channel();

        let queue2 = queue.clone();
        let handle = thread::spawn(move|| {
            let pcm = PCM::new(&*config.audio_device, Direction::Playback, false)
                .map_err(|e| DecideError::Component { source: e.into() }).unwrap();

            let hwp = HwParams::any(&pcm)
                .map_err(|e| DecideError::Component { source: e.into() }).unwrap();
            hwp.set_channels(config.channel)
                .map_err(|e| DecideError::Component { source: e.into() }).unwrap();
            hwp.set_rate(config.sample_rate, ValueOr::Nearest)
                .map_err(|e| DecideError::Component { source: e.into() }).unwrap();
            hwp.set_access(Access::RWInterleaved)
                .map_err(|e| DecideError::Component { source: e.into() }).unwrap();
            hwp.set_format(Format::s16())
                .map_err(|e| DecideError::Component { source: e.into() }).unwrap();
            pcm.hw_params(&hwp)
                .map_err(|e| DecideError::Component { source: e.into() }).unwrap();

            let swp = pcm.sw_params_current()
                .map_err(|e| DecideError::Component { source: e.into() }).unwrap();
            swp.set_start_threshold(hwp.get_buffer_size().unwrap())
                .map_err(|e| DecideError::Component { source: e.into() }).unwrap();
            pcm.sw_params(&swp)
                .map_err(|e| DecideError::Component { source: e.into() }).unwrap();
            let io = pcm.io_i16()
                .map_err(|e| DecideError::Component { source: e.into() }).unwrap();

            'stim: loop {
                //shutdown
                if sd_rx.try_recv().unwrap_err() == std_mpsc::TryRecvError::Disconnected {
                    pcm.drop().map_err(|e| DecideError::Component { source: e.into() }).unwrap();
                    break};

                let stim = audio_id.lock().unwrap();
                let p = playback.lock().unwrap().clone();
                if p == PlayBack::Playing {
                    if stim.ends_with("wav")  {
                        let stim_queue = queue2.lock().unwrap();
                        let stim_name = OsString::from(stim.clone());
                        let mut elapsed = elapsed.lock().unwrap();
                        pcm.hw_params(&hwp)
                            .map_err(|e| DecideError::Component { source: e.into() }).unwrap();
                        let data = stim_queue.get(&*stim_name).unwrap();
                        io.writei(data).map_err(|e| DecideError::Component { source: e.into() }).unwrap();
                        tracing::debug!("Begin playback");
                        let timer = std::time::Instant::now();
                        'playback: while pcm.state() == State::Running {
                            //shutdown check
                            if sd_rx.try_recv().unwrap_err() == std_mpsc::TryRecvError::Disconnected {
                                pcm.drop().map_err(|e| DecideError::Component { source: e.into() }).unwrap();
                                break 'stim}

                            let p = playback.try_lock().unwrap().clone();
                            match p {
                                PlayBack::Next => {
                                    pcm.drop()
                                        .map_err(|e| DecideError::Component { source: e.into() }).unwrap();
                                    let duration = prost_types::Duration::try_from(timer.elapsed())
                                                            .map_err(|e| println!("Could not convert protobuf duration to std duration")).unwrap();
                                    *elapsed = Some(duration.clone());
                                    let mut playback = playback.lock().unwrap();
                                    *playback = PlayBack::Playing;
                                    let state = Self::State {
                                        audio_id: audio_id.lock().unwrap().clone(),
                                        playback: PlayBack::Playing as i32,
                                        elapsed: Some(duration)
                                    };
                                    let message = Any {
                                        value: state.encode_to_vec(),
                                        type_url: Self::STATE_TYPE_URL.into(),
                                    };
                                    block_on(sender.send(message))
                                        .map_err(|e| DecideError::Component { source: e.into() }).unwrap();
                                    tracing::debug!("Next audio, elapsed time recorded");
                                    continue 'stim}
                                PlayBack::Stop => {
                                    let duration = prost_types::Duration::try_from(timer.elapsed())
                                                            .map_err(|e| println!("Could not convert protobuf duration to std duration")).unwrap();
                                    *elapsed = Some(duration.clone());
                                    pcm.drop().unwrap();
                                    let state = Self::State {
                                        audio_id: String::from("None"),
                                        playback: PlayBack::Stop as i32,
                                        elapsed: Some(duration)
                                    };
                                    let message = Any {
                                        value: state.encode_to_vec(),
                                        type_url: Self::STATE_TYPE_URL.into(),
                                    };
                                    block_on(sender.send(message))
                                        .map_err(|e| DecideError::Component { source: e.into() }).unwrap();
                                    tracing::debug!("Stop audio,  elapsed time recorded");
                                    continue 'stim}
                                PlayBack::Playing => {continue 'playback}
                            };
                        }
                    } else {
                        tracing::error!("Playback set to Playing but filename is invalid");
                        thread::sleep(Duration::from_millis(1000)); continue 'stim}
                } else {
                    thread::sleep(Duration::from_millis(1000)); continue
                }
            }
        });

        self.shutdown = Some((handle, sd_tx));
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
            tracing::trace!("Sound_Alsa state changed");
        });
        Ok(())
    }

    fn set_parameters(&mut self, _params: Self::Params) -> decide_protocol::Result<()> {
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
        Self::Params {}

    }

    async fn shutdown(&mut self) {
        if let Some((handle, sender)) = self.shutdown.take() {
            drop(sender);
            handle.join()
                //disable mapping here due to multiple errors
                //.map_err(|e| DecideError::Component { source: e.into() })
                .unwrap()
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

