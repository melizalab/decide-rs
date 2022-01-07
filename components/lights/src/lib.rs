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

pub mod proto {
    include!(concat!(env!("OUT_DIR"), "/_.rs"));
}

#[allow(dead_code)]
#[derive(Deserialize, Debug)]
pub struct LightsConfig {
    pin: u8,
}

pub struct Lights {
    on: Arc<AtomicBool>,
    blink: Arc<AtomicBool>,
    state_sender: mpsc::Sender<Any>,
}

#[async_trait]
impl Component for Lights {
    type State = proto::State;
    type Params = proto::Params;
    type Config = LightsConfig;
    const STATE_TYPE_URL: &'static str = "melizalab.org/proto/lights_state";
    const PARAMS_TYPE_URL: &'static str = "melizalab.org/proto/lights_params";

    fn new(config: Self::Config, state_sender: mpsc::Sender<Any>) -> Self {
        println!("Lights with config {:?}", config);
        Lights {
            on: Arc::new(AtomicBool::new(false)),
            blink: Arc::new(AtomicBool::new(false)),
            state_sender,
        }
    }

    async fn init(&mut self, _config: Self::Config) {
        let blink = Arc::clone(&self.blink);
        let on = Arc::clone(&self.on);
        let sender = self.state_sender.clone();
        tokio::spawn(async move {
            loop {
                if blink.load(Ordering::Acquire) {
                    tracing::debug!("lights changing state");
                    let old_state = on.fetch_xor(true, Ordering::AcqRel);
                    let new_state = !old_state;
                    let state = Self::State { on: new_state };
                    let message = Any {
                        value: state.encode_to_vec(),
                        type_url: Self::STATE_TYPE_URL.into(),
                    };
                    sender.send(message).await.unwrap();
                }
                sleep(Duration::from_millis(100)).await;
            }
        });
    }

    fn change_state(&mut self, state: Self::State) -> Result<(), DecideError> {
        self.on.store(state.on, Ordering::Release);
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

    fn set_parameters(&mut self, params: Self::Params) -> Result<(), DecideError> {
        self.blink.store(params.blink, Ordering::Release);
        Ok(())
    }

    fn get_parameters(&self) -> Self::Params {
        Self::Params {
            blink: self.blink.load(Ordering::Acquire),
        }
    }

    fn get_state(&self) -> Self::State {
        Self::State {
            on: self.on.load(Ordering::Acquire),
        }
    }
}
