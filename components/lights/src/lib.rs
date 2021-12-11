use async_trait::async_trait;
use decide_proto::{error::DecideError, Component};
use prost::Message;
use prost_types::Any;
use serde::Deserialize;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use tokio::{
    self,
    sync::mpsc,
    time::{sleep, Duration},
};

pub mod lights {
    include!(concat!(env!("OUT_DIR"), "/_.rs"));
}

#[derive(Deserialize, Debug)]
pub struct LightsConfig {
    pin: u8,
}

pub struct Lights {
    on: Arc<AtomicBool>,
    blink: Arc<AtomicBool>,
}

#[async_trait]
impl Component for Lights {
    type State = lights::State;
    type Params = lights::Params;
    type Config = LightsConfig;
    const STATE_TYPE_URL: &'static str = "melizalab.org/proto/lights_state";
    const PARAMS_TYPE_URL: &'static str = "melizalab.org/proto/lights_params";

    fn new(config: Self::Config) -> Self {
        println!("Lights with config {:?}", config);
        Lights {
            on: Arc::new(AtomicBool::new(false)),
            blink: Arc::new(AtomicBool::new(false)),
        }
    }

    async fn init(&self, sender: mpsc::Sender<Any>) {
        let blink = self.blink.clone();
        let on = self.on.clone();
        tokio::spawn(async move {
            loop {
                if blink.load(Ordering::Relaxed) {
                    let old_state = on.fetch_xor(true, Ordering::Relaxed);
                    let new_state = !old_state;
                    let state = Self::State { on: new_state };
                    let message = Any {
                        value: state.encode_to_vec(),
                        type_url: Self::STATE_TYPE_URL.into(),
                    };
                    sender.send(message).await.unwrap();
                    sleep(Duration::from_millis(100)).await;
                }
            }
        });
    }

    fn change_state(&mut self, state: Self::State) -> Result<(), DecideError> {
        self.on.store(state.on, Ordering::Relaxed);
        Ok(())
    }

    fn set_parameters(&mut self, params: Self::Params) -> Result<(), DecideError> {
        self.blink.store(params.blink, Ordering::Relaxed);
        Ok(())
    }

    fn get_parameters(&self) -> Self::Params {
        Self::Params {
            blink: self.blink.load(Ordering::Relaxed),
        }
    }

    fn get_state(&self) -> Self::State {
        Self::State {
            on: self.on.load(Ordering::Relaxed),
        }
    }
}
