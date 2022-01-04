use decide_proto::{Component, error::DecideError};
use prost::Message;
use prost_types::Any;
use std::sync::{Mutex, Arc, atomic::{AtomicBool, AtomicU8, Ordering}};
use std::thread;
use serde::Deserialize;
use async_trait::async_trait;
use futures::{SinkExt, StreamExt};
use tokio::{self,
            sync::mpsc,
            time::{//sleep,
                   Duration}
};
use regex::Regex;
use simple_logger::*; //TODO: is logger necessary
use tmq::{Context, publish, subscribe, request, dealer};

pub struct AudioPlayer {
    server: Arc<AtomicBool>, //'running' or 'stopped'
    playing: Arc<AtomicBool>,
    stim_path: Arc<Mutex<String>>,
    timeout: Arc<AtomicU8>,
    state_sender: Option<mpsc::Sender<Any>>,
}
pub mod sound {
    include!(concat!(env!("OUT_DIR"), "/_.rs"));
}

#[async_trait]
impl Component for AudioPlayer {
    type State = sound::State;
    type Params = sound::Params;
    type Config = Config;
    const STATE_TYPE_URL: &'static str = "melizalab.org/proto/sound_state";
    const PARAMS_TYPE_URL: &'static str = "melizalab.org/proto/sound_params";

    fn new(_config: Self::Config) -> Self {
        AudioPlayer {
            server: Arc::new(AtomicBool::new(false)),
            playing: Arc::new(AtomicBool::new(false)),
            stim_path: Arc::new(Mutex::new(String::from(".wav"))),
            timeout: Arc::new(AtomicU8::new(Default::default())),
            state_sender: None,
        }
    }

    async fn init(&mut self, _config: Self::Config, state_sender: mpsc::Sender<Any>) {
        self.state_sender = Some(sender.clone());
        let server = self.server.clone();
        let playing = self.playing.clone();
        let mut stim_path = self.stim_path.clone();

        tokio::spawn(async move {
            let jstim_contxt = tmq::Context::new();
            let mut dealer_soc = dealer(&jstim_contxt)
                .connect(Self::Params::req).unwrap();
            let mut sub_soc = subscribe(&jstim_contxt)
                .connect(Self::Params::sub).unwrap()
                .subscribe(b"").unwrap();
            while let Some(multprt) = sub_soc.next().await {
                tracing::trace!("Jstim subscription msg received");
                let mut stim_path = stim_path.lock().unwrap();
                let mut multipart = multprt.unwrap()
                    .iter()
                    .map(|part| part.as_str())
                    .collect::<Vec<&str>>();
                if multipart[0] == "PLAYING" {
                    server.store(true, Ordering::Release);
                    playing.store(true, Ordering::Release);
                    *stim_path = String::from(multipart[1]);
                } else {
                    match multipart[0] {
                        "DONE" => {
                            server.store(true, Ordering::Release);
                            playing.store(false, Ordering::Release);
                            *stim_path = String::from("None");
                        },
                        "STOPPING" => {
                            tracing::info!("jstimserver shut down; stimuli won't be playing");
                            server.store(false, Ordering::Release);
                            playing.store(false, Ordering::Release);
                            *stim_path = String::from("None");
                        },
                        "STARTING" => {
                            tracing::info!("jstimserver reconnected");
                            server.store(true, Ordering::Release);
                            playing.store(true, Ordering::Release);
                            *stim_path = AudioPlayer::update_stims(&dealer_soc);
                        },
                        _ => {tracing::error!("Unexpected msg from jstimserver {:?}", multipart)}
                    }
                }
                let mut state = Self::State {
                    server: server.load(Ordering::Acquire),
                    playing: playing.load(Ordering::Acquire),
                    stim_path: stim_path.clone(),
                };
                let message = Any {
                    value: state.encode_to_vec(),
                    type_url: Self::STATE_TYPE_URL.into(),
                };
                state_sender.send(message).await.unwrap();
            }
        });
    }

    fn change_state(&mut self, state: Self::State) -> decide_proto::Result<()> {
        self.server.store(state.server, Ordering::Release);
        self.playing.store(state.playing, Ordering::Release);
        let mut stim_path = self.stim_path.try_lock().unwrap();
        *stim_path = state.stim_path;
        let sender = self.state_sender.as_mut().cloned().unwrap();
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

    fn set_parameters(&mut self, params: Self::Params) -> decide_proto::Result<()> {
        self.timeout.store(params.timeout, Ordering::Release);
        Ok(())
    }

    fn get_state(&self) -> Self::State {
        let stim_path = self.stim_path.try_lock().unwrap();
        Self::State {
            server: self.server.load(Ordering::Acquire),
            playing: self.playing.load(Ordering::Acquire),
            stim_path: stim_path.clone(),
        }
    }

    fn get_parameters(&self) -> Self::Params {
        Self::Params {
            timeout: self.timeout.load(Ordering::Acquire),
        }
    }
}

impl AudioPlayer {
    fn update_stims(mut dealer: &dealer::Dealer) -> String{
        dealer.send("STIMLIST").await.unwrap();
        let mut jstim_resp = dealer.next().await.unwrap().unwrap();
        let mut stims = Vec::new();
        while let Some(message) = jstim_resp.pop_front().unwrap() {
            stims.push(message.as_str())
        };
        tracing::info!("Received stimulus list of {:?} from jstim server", stims.len());
        String::from(stims[1])
    }
}


#[derive(Deserialize)]
pub struct Config {

}