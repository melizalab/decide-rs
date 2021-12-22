use decide_proto::{Component};
use prost::Message;
use prost_types::Any;
use std::sync::{Arc, atomic::{AtomicBool, AtomicU8, Ordering}};
use std::thread;
use serde::Deserialize;
use async_trait::async_trait;
use futures::{SinkExt, StreamExt};
use tokio::{self,
            sync::mpsc::{Sender},
            time::{//sleep,
                   Duration}
};
use regex::Regex;
use simple_logger::*; //TODO: is logger necessary
use tmq::{Context, publish, subscribe, request, dealer};

pub struct AudioPlayer {
    server: Arc<AtomicBool>, //'running' or 'stopped'
    playing: Arc<AtomicBool>,
    stim_path: Arc<&'static str>,
    timeout: Arc<AtomicU8>
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
            stim_path: Arc::new(".wav"),
            timeout: Arc::new(AtomicU8::new(Default::default()))
        }
    }

    async fn init(&self, _config: Self::Config, state_sender: Sender<Any>) {
        tokio::spawn(async move {
            let jstim_contxt = tmq::Context::new();
            let mut dealer_soc = dealer(&jstim_contxt)
                .connect(Self::Params::req).unwrap();
            let mut sub_soc = subscribe(&jstim_contxt)
                .connect(Self::Params::sub).unwrap()
                .subscribe(b"").unwrap();
            while let Some(multprt) = sub_soc.next().await {
                let mut state = Self::State {server:true, playing: true, stim_path: "none"};
                let mut multipart = multprt.unwrap()
                    .iter()
                    .map(|part| part.as_str())
                    .collect::<Vec<&str>>();
                if multipart[0] == "PLAYING" {
                    state = Self::State { server:true, playing:true, stim_path: multipart[1] };
                } else {
                    match multipart[0] {
                        "DONE" => {
                            state = Self::State { server:true, playing:false, stim_path };},
                        "STOPPING" => {
                            println!("jstimserver shut down; stimuli won't be playing");
                            state = Self::State { server:false, playing:false, stim_path };},
                        "STARTING" => {
                            println!("jstimserver reconnected");
                            state = AudioPlayer::update_stims(&dealer_soc);}
                        _ => {println!("Unexpected msg from jstimserver {:?}", multipart)}
                    }
                }
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
        self.stim_path = state.stim_path;
        Ok(())
    }

    fn set_parameters(&mut self, params: Self::Params) -> decide_proto::Result<()> {
        self.timeout.store(params.timeout, Ordering::Release);
        Ok(())
    }

    fn get_state(&self) -> Self::State {
        Self::State {
            server: self.server.load(Ordering::Acquire),
            playing: self.playing.load(Ordering::Acquire),
            stim_path: self.stim_path.clone(),
        }
    }

    fn get_parameters(&self) -> Self::Params {
        Self::Params {
            timeout: self.timeout.load(Ordering::Acquire),
        }
    }
}

impl AudioPlayer {
    fn update_stims(mut dealer: &dealer::Dealer) -> Self::State{
        dealer.send("STIMLIST").await.unwrap();
        let mut jstim_resp = dealer.next().await.unwrap().unwrap();
        let mut stims = Vec::new();
        while let Some(message) = jstim_resp.pop_front().unwrap() {
            stims.push(message.as_str())
        };

        println!("Received stimulus list of {:?} from jstim server", stims.len());
        Self::State {
            server: true,
            playing: true,
            stim_path: stims[1]
        }
    }
}


#[derive(Deserialize)]
pub struct Config {

}