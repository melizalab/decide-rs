use async_trait::async_trait;
use decide_protocol::{error::DecideError,
                      Component};
use gpio_cdev::{Chip, AsyncLineEventHandle,
                EventRequestFlags, EventType,
                LineRequestFlags};
use futures::stream::StreamExt;
use prost::Message;
use prost_types::Any;
use serde::Deserialize;
use thiserror::Error;
use tokio::{self, time::{Duration}, sync::mpsc::Sender, task::JoinHandle};

pub struct GpioSwitch {
    state_sender: Sender<Any>,
    task_handle: Option<JoinHandle<()>>
}

#[derive(Deserialize)]
pub struct SwitchConfig {
    chip: String,
    line: u32,
}

pub mod proto {
    include!(concat!(env!("OUT_DIR"), "/_.rs"));
}

#[async_trait]
impl Component for GpioSwitch {
    type State = proto::SwitchState;
    type Params = proto::SwitchParams;
    type Config = SwitchConfig;
    const STATE_TYPE_URL: &'static str = "type.googleapis.com/SwitchState";
    const PARAMS_TYPE_URL: &'static str =  "type.googleapis.com/SwitchParams";

    fn new(_config: Self::Config, sender: Sender<Any>) -> Self {
        GpioSwitch {
            state_sender: sender,
            task_handle: None}
    }

    async fn init(&mut self, config: Self::Config) {
        let sender = self.state_sender.clone();
        self.task_handle = Some(tokio::spawn(async move {
            let mut chip = Chip::new(config.chip.clone())
                .map_err(|_e| DecideError::Component { source:
                    SwitchError::GpioChipError {dev: config.chip}.into()
                }).unwrap();
            let gpio_interrupt_line = chip.get_line(config.line.clone())
                .map_err(|_e| DecideError::Component { source:
                    SwitchError::GpioLineReqError {line: config.line}.into()
                }).unwrap();
            let mut interrupt = AsyncLineEventHandle::new(
                gpio_interrupt_line.events(LineRequestFlags::INPUT,
                                        EventRequestFlags::BOTH_EDGES,      // we're interested in capturing FALLING_EDGE
                                        "lever-interrupt"     // but oddly setting flags to FALLING_EDGE still
                                        )             // gives us both edges.
                    .map_err(|_e| DecideError::Component {source:
                        SwitchError::GpioFlagReqError {line: config.line,
                                          flag: "INPUT".to_string()}.into() })
                    .unwrap())
                .map_err(|_e: gpio_cdev::Error| DecideError::Component { source:
                    SwitchError::GpioAsyncLineError {line: config.line}.into()})
                .unwrap();

            loop {
                match interrupt.next().await {
                    Some(event) => {
                        match event.unwrap().event_type() {
                            EventType::RisingEdge => {
                                tracing::info!("lever pulled!");
                                let state = Self::State{ switch: true };
                                Self::send_state(&state, &sender).await;
                            }
                            EventType::FallingEdge => {continue}
                        }
                    }
                    None => {tracing::error!("lever gpio interrupted - no event?");continue},
                }
                let mut debounce = false;
                while !debounce {
                    tokio::select! {
                        _ = interrupt.next() => {}
                        _ = tokio::time::sleep(Duration::from_micros(20)) => {debounce=true}
                    }}

            }
        }));
        tracing::info!("lever initiated")
    }

    fn change_state(&mut self, _state: Self::State) -> decide_protocol::Result<()> {
        tracing::error!("Lever change_state not implemented. Make sure your script isn't using it without good reason");
        Ok(())
    }

    fn set_parameters(&mut self, _params: Self::Params) -> decide_protocol::Result<()> {
        Ok(())
    }

    fn get_state(&self) -> Self::State {
        Self::State{ switch: false }
    }

    fn get_parameters(&self) -> Self::Params {
        Self::Params{}
    }

    async fn send_state(state: &Self::State, sender: &Sender<Any>) {
        tracing::debug!("emiting state change");
        sender.send(Any {
            type_url: String::from(Self::STATE_TYPE_URL),
            value: state.encode_to_vec(),
        }).await.map_err(|_e| DecideError::Component { source:
            SwitchError::SendError.into() }).unwrap();
    }

    async fn shutdown(&mut self) {
        if let Some(task_handle) = self.task_handle.take() {
            task_handle.abort();
            assert!(task_handle.await.unwrap_err().is_cancelled());
        }
    }
}


#[derive(Error, Debug)]
pub enum SwitchError {
    #[error("could not initialize gpio device {dev:?}")]
    GpioChipError{dev:String},
    #[error("could not request lines {line:?} from gpio device")]
    GpioLineReqError{line: u32},
    #[error("could not set gpio lines {line:?} to mode {flag:?}")]
    GpioFlagReqError{line: u32, flag: String},
    #[error("could not get async gpio line {line:?}")]
    GpioAsyncLineError{line: u32},
    #[error("could not send state update")]
    SendError,
}