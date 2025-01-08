use std::collections::HashMap;
use std::ffi::OsString;
use std::sync::{Arc, mpsc as std_mpsc, Mutex};
use std::sync::atomic::{AtomicU32, Ordering};
use std::thread;

use alsa::{Direction, pcm::PCM};
use async_trait::async_trait;
use atomic_wait::{wait, wake_all};
use futures::executor::block_on;
use prost::Message;
use prost_types::Any;
use tokio::{self, sync::mpsc::Sender as tkSender};
use tokio::sync::mpsc;
use decide_protocol::{Component,
                      error::DecideError
};
use crate::tasklets::PlaybackError;

mod tasklets;

pub struct AlsaPlayback {
    conf_path: Arc<Mutex<String>>,
    audio_id: Arc<Mutex<String>>,
    audio_count: Arc<AtomicU32>,
    channels: bool,
    sample_rate: Arc<AtomicU32>,
    playback: Arc<AtomicU32>, //pause or resume
    frames: Arc<AtomicU32>,
    playback_queue: Arc<Mutex<HashMap<OsString, (Vec<i16>, u32)>>>,
    state_sender: tkSender<Any>,
    import_switch: Arc<AtomicU32>,
    shutdown: Option<(std::thread::JoinHandle<()>,std_mpsc::Sender<bool>)>,
}

#[async_trait]
impl Component for AlsaPlayback {
    type State = proto::SaState;
    type Params = proto::SaParams;
    type Config = tasklets::Config;
    const STATE_TYPE_URL: &'static str = "type.googleapis.com/SaState";
    const PARAMS_TYPE_URL: &'static str = "type.googleapis.com/SaParams";

    fn new(_config: Self::Config, state_sender: tkSender<Any>) -> Self {

        AlsaPlayback{
            audio_id: Arc::new(Mutex::new(String::from("None"))),
            conf_path: Arc::new(Mutex::new(String::from("None"))),
            audio_count: Arc::new(AtomicU32::new(0)),
            channels: false, //mono
            sample_rate: Arc::new(AtomicU32::new(0)),
            playback: Arc::new(AtomicU32::new(0)), //0: Stopped, 1: Playing
            frames: Arc::new(AtomicU32::new(0)),
            playback_queue: Arc::new(Mutex::new(HashMap::new())),
            state_sender,
            import_switch: Arc::new(AtomicU32::new(1)),
            shutdown: None,
        }
    }

    async fn init(&mut self, config: Self::Config) {
        // Playback state message
        let audio_id = self.audio_id.clone();
        let playback = self.playback.clone();
        let frames = self.frames.clone();
        // Playback thread communication
        let sender = self.state_sender.clone();
        let queue = self.playback_queue.clone();
        let import_switch2 = self.import_switch.clone();
        let (sd_tx, sd_rx) = std_mpsc::channel();

        self.channels = match config.channels {
            1 => {false}
            2 => {true}
            _ => {
                tracing::error!("invalid number of audio channels specified in config {:?}", config.channels);
                false
            }
        };
        self.sample_rate.store(config.sample_rate, Ordering::Release);
        //playback thread
        let handle = thread::spawn(move || {
            let audio_dev = PCM::new(&config.audio_device.clone(), Direction::Playback, false)
                .map_err(|_e| DecideError::Component { source:
                    PlaybackError::HardwareSetupError {tag: "PCM initialization".to_string()}.into()
                }).unwrap();
            tracing::debug!("sound-alsa - pcm device created on {:?}", config.audio_device.clone());
            tasklets::get_hw_config(&audio_dev, &config).unwrap();

            let mixer = alsa::mixer::Mixer::new(&config.card.clone(), false)
                .map_err(|_e| DecideError::Component { source:
                    PlaybackError::SoftwareSetupError {tag: "mixer initialization".to_string()}.into()
                }).unwrap();
            let selem_id = alsa::mixer::SelemId::new("PCM", 0);
            let selem = mixer.find_selem(&selem_id).ok_or_else(|| {
                format!(
                    "Couldn't find selem with name '{}'.",
                    selem_id.get_name().unwrap_or("unnamed")
                )
            }).unwrap();
            selem.set_playback_volume_range(0, 100)
                .map_err(|_e| DecideError::Component { source:
                    PlaybackError::SoftwareSetupError {tag: "volume range".to_string()}.into()
                }).unwrap();
            selem.set_playback_volume_all(config.volume as i64)
                .map_err(|_e| DecideError::Component { source:
                    PlaybackError::SoftwareSetupError {tag: "volume".to_string()}.into()
                }).unwrap();
            drop(mixer);

            // let mut mmap = audio_dev.direct_mmap_playback::<i16>();
            let mut io = audio_dev.io_i16()
                .map_err(|_e| DecideError::Component { source:
                    PlaybackError::SoftwareSetupError {tag: "IO: 16bit".to_string()}.into()
                }).unwrap();

            'stim: loop {
                // Check shutdown
                if sd_rx.try_recv().unwrap_err() == std_mpsc::TryRecvError::Disconnected {
                    audio_dev.drain().expect("Draining PCM for shutdown.");
                    audio_dev.drop().expect("Dropping PCM for shutdown");
                    break};

                // block & await completion of import - change to 0
                wait(&import_switch2, 1);
                // playback loop
                // Block and await playback change to 1
                wait(&playback, 0);
                tracing::info!("sound-alsa: starting playback!");
                let stim = audio_id.lock()
                    .map_err(|_e| DecideError::Component {source:
                        PlaybackError::PlaybackMemoryError {tag: "stimulus id lock".to_string()}.into()
                    }).unwrap();
                let stim_name = OsString::from(stim.clone());
                let stim_queue = queue.lock()
                    .map_err(|_e| DecideError::Component {source:
                        PlaybackError::PlaybackMemoryError {tag: "playback queue lock".to_string()}.into()
                    }).unwrap();
                let data = stim_queue.get(&*stim_name)
                    .ok_or( DecideError::Component { source:
                        PlaybackError::StimulusMissing {name: stim.clone()}.into()
                    }).unwrap();

                let frame_count = data.1.clone();
                frames.store(frame_count.clone(), Ordering::Release);

                block_on(
                    Self::send_state(
                        &Self::State {
                            audio_id: stim_name.clone().into_string().unwrap(),
                            playback: true,
                            frame_count: frame_count.clone() },
                        &sender
                ));
                match audio_dev.prepare() {
                    Ok(n) => n,
                    Err(e) => {
                        tracing::warn!("sound-alsa failed to prepare for playback\
                                        , recovering from {:?}", e);
                        audio_dev.recover(e.errno() as std::os::raw::c_int, true)
                            .map_err(|_e| DecideError::Component {source:
                                PlaybackError::PlaybackError {tag: "recover".to_string()}.into()
                            }).unwrap();
                    }
                }
                if !tasklets::playback_io(&audio_dev, &mut io, &data.0,
                                          frame_count, &playback).unwrap() {
                    continue 'stim
                }
                tracing::info!("sound-alsa: playback complete!");
                // playback finished without interruption. Send info about completed stim
                block_on(
                    Self::send_state(&Self::State {
                        audio_id: stim_name.clone().into_string().unwrap(),
                        playback: false,
                        frame_count: frame_count.clone()
                    }, &sender)
                );
                playback.store(0, Ordering::Release);
                //audio_dev.drop().unwrap();
            }
        });
        tracing::info!("sound-alsa initiated.");
        self.shutdown = Some((handle, sd_tx));
    }

    fn change_state(&mut self, state: Self::State) -> decide_protocol::Result<()> {
        let current_pb = self.playback.load(Ordering::Acquire);
        //compare sent playback control signal against current control
        match state.playback  {
            true => {
                match current_pb {
                    1 => {tracing::error!("Requested stim while already playing. Send next or stop first.")}
                    0 => {
                        let mut audio_id = self.audio_id.lock()
                            .map_err(|_e| DecideError::Component {source:
                                PlaybackError::PlaybackMemoryError {tag: "stimulus id lock".to_string()}.into()
                            })?; // this will block if playback is underway
                        *audio_id = state.audio_id;
                        self.playback.store(1, Ordering::Release);
                        let pb = self.playback.as_ref();
                        wake_all(pb);
                    }
                    _ => {tracing::error!("Invalid Playback value detected {:?}", current_pb)}
                }
            }
            false => {
                match current_pb {
                    1 => { // playback requested interruption
                        self.playback.store(0, Ordering::Release);
                        tracing::debug!("sound-alsa requested interrupt.");
                    }
                    0 => {
                        tracing::info!("sound-alsa requested to stop while already stopped.");
                    }
                    _ => {tracing::error!("sound-alsa invalid playback value detected {:?}", current_pb)}
                }
            }
        };
        //we do not send actual state change PUB messages in change_state(), since it's more important
        //that PUB msgs come from the playback thread
        Ok(())
    }

    fn set_parameters(&mut self, params: Self::Params) -> decide_protocol::Result<()> {
        //stop playback & initiate import when params are changed:
        self.playback.store(0, Ordering::Release);

        //import
        let current_conf: String = self.conf_path.lock()
            .map_err(|_e| DecideError::Component {source:
                PlaybackError::PlaybackMemoryError {tag: "config path lock".to_string()}.into()
            })?.drain(..).collect();

        tracing::info!("sound-alsa current config file :{:?}, provided config file {:?}",
            current_conf.clone(), params.conf_path);

        if params.conf_path != current_conf {
            let mut stim_queue = self.playback_queue.lock()
                .map_err(|_e| DecideError::Component {source:
                    PlaybackError::PlaybackMemoryError {tag: "stimulus queue lock".to_string()}.into()
                })?; // this will block if playback is underway
            stim_queue.clear();
            std::mem::drop(stim_queue);
            self.import_switch.store(1, Ordering::Release);
            let import_switch = self.import_switch.clone();
            let queue = self.playback_queue.clone();
            let mut current_conf = self.conf_path.lock()
                .map_err(|_e| DecideError::Component {source:
                    PlaybackError::PlaybackMemoryError {tag: "config path lock".to_string()}.into()
                })?; // this will block if playback is underway
            *current_conf = params.conf_path.clone();
            tasklets::import_audio(import_switch, queue, self.channels.clone(),
                                   params.conf_path,self.audio_count.clone())
        };
        Ok(())
    }

    fn get_state(&self) -> Self::State {
        Self::State {
            audio_id: self.audio_id.lock()
                .map_err(|_e| DecideError::Component {source:
                    PlaybackError::PlaybackMemoryError {tag: "stimulus id lock".to_string()}.into()
                }).unwrap().clone(),
            playback: if self.playback.load(Ordering::Acquire)==1 {true} else {false},
            frame_count: self.frames.load(Ordering::Acquire),
        }
    }

    fn get_parameters(&self) -> Self::Params {
        Self::Params {
            conf_path: self.conf_path.lock()
                .map_err(|_e| DecideError::Component {source:
                    PlaybackError::PlaybackMemoryError {tag: "config path lock".to_string()}.into()
                }).unwrap().clone(),
            audio_count: self.audio_count.load(Ordering::Relaxed),
            sample_rate: self.sample_rate.load(Ordering::Relaxed),
        }
    }

    async fn send_state(state: &Self::State, sender: &mpsc::Sender<Any>) {
        tracing::debug!("emitting state change");
        sender.send(Any {
            type_url: String::from(Self::STATE_TYPE_URL),
            value: state.encode_to_vec(),
        }).await.map_err(|_e| DecideError::Component { source:
            PlaybackError::SendError.into()
        }).unwrap();
    }

    async fn shutdown(&mut self) {
        if let Some((handle, sender)) = self.shutdown.take() {
            drop(sender);
            drop(handle);
        }
    }
}

pub mod proto {
    include!(concat!(env!("OUT_DIR"), "/_.rs"));
}