use decide_proto::{Component};
use prost::Message;
use prost_types::Any;
use std::sync::{Arc, atomic::{AtomicBool, AtomicU8, Ordering}};
use std::thread;
use serde::Deserialize;
use async_trait::async_trait;
use tokio::{self,
            sync::mpsc::{Sender},
            time::{//sleep,
                   Duration}
};
use tmq;

struct AudioPlayer {
    server: Arc<AtomicBool>,
    playing: Arc<AtomicBool>,
    stimulus: &'static str,
    timeout: Arc<AtomicU8>
}

#[async_trait]
impl Component for AudioPlayer {
    type State = sound::State;
    type Params = sound::Params;
    type Config = Config;
    const STATE_TYPE_URL: &'static str = "melizalab.org/proto/sound_state";
    const PARAMS_TYPE_URL: &'static str = "melizalab.org/proto/sound_params";

    fn new(config: Self::Config) -> Self {
        AudioPlayer {
            server: Arc::new(AtomicBool::new(false)),
            playing: Arc::new(AtomicBool::new(false)),
            stimulus: ".wav",
            timeout: Arc::new(AtomicU8::new(Self::Params::timeout))
        }
    }

    async fn init(&self, config: Self::Config, state_sender: Sender<Any>) {
        todo!()
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

#[derive(Deserialize)]
pub struct Config {

}