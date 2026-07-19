use async_trait::async_trait;
use futures::stream::StreamExt;
use decide_protocol::{error::DecideError,
                      Component};
use gpio_cdev::{AsyncLineEventHandle, Chip,
                EventRequestFlags,
                EventType,
                LineRequestFlags,
                MultiLineHandle
};
use prost::Message;
use prost_types::Any;
use serde::Deserialize;
use std::path::Path;
use thiserror::Error;
use tokio::sync::mpsc::Sender;
use tokio::{self, task::JoinHandle, time::{Duration}};

pub struct FbKeys {
    state_sender: Sender<Any>,
    task_handle: Option<JoinHandle<()>>,
}

#[async_trait]
impl Component for FbKeys {
    type State = proto::FbKeyState;
    type Params = proto::FbKeyParams;
    type Config = FbKeyConfig;
    const STATE_TYPE_URL: &'static str = "type.googleapis.com/FbKeyState";
    const PARAMS_TYPE_URL: &'static str = "type.googleapis.com/FbKeyParams";

    fn new(_config: Self::Config, sender: Sender<Any>) -> Self {

        if !Path::new("/sys/class/i2c-adapter/i2c-1/1-0020").exists() {
            panic!("{}", FinchBoardError::MissingDevice);
        };

        FbKeys {
            state_sender: sender,
            task_handle: None,
        }
    }

    async fn init(&mut self, config: Self::Config) {

        let sender = self.state_sender.clone();

        self.task_handle = Some(tokio::spawn( async move {
            let mut int_chip = Chip::new(&config.interrupt_chip)
                .map_err(|_e| DecideError::Component { source: 
                    FinchBoardError::GpioChipError {dev: config.interrupt_chip}.into()
                }).unwrap();
            let int_line  = int_chip.get_line(config.interrupt_offset.clone())
                .map_err(|_e| DecideError::Component { source:
                    FinchBoardError::GpioLineReqError {pos: "interrupt".to_string(), tech: "key".to_string()}.into()
                }).unwrap();
            let mut interrupt = AsyncLineEventHandle::new(
                int_line.events(LineRequestFlags::INPUT,
                                        EventRequestFlags::BOTH_EDGES,
                                        "finchboard interrupt")
                    .map_err(|_e| DecideError::Component { source: 
                        FinchBoardError::GpioFlagReqError { pos: "interrupt".to_string(),
                                                            tech: "key".to_string(),
                                                            flag: "INPUT".to_string()}.into()    
                    }).unwrap()
            ).map_err(|_e| DecideError::Component { source: 
                FinchBoardError::GpioAsyncLineError { line: vec![config.interrupt_offset as u8]}.into()
            }).unwrap(); 

            let mut dev_chip = Chip::new(&config.device_chip)
                .map_err(|_e| DecideError::Component { source:
                    FinchBoardError::GpioChipError { dev: config.device_chip }.into()
                }).unwrap();
            let left_handles: MultiLineHandle = dev_chip.get_lines(&config.left_offsets)
                .map_err(|_e| DecideError::Component { source:
                    FinchBoardError::GpioLineReqError {pos: "left".to_string(), tech: "key".to_string()}.into() })
                .unwrap()
                .request(LineRequestFlags::INPUT, &[0,0], "left_beam_breaks")
                .map_err(|_e| DecideError::Component { source:
                    FinchBoardError::GpioFlagReqError {
                        pos: "left".to_string(),
                        tech: "key".to_string(),
                        flag: "INPUT".to_string()}.into() })
                .unwrap();
            let right_handles: MultiLineHandle = dev_chip.get_lines(&config.right_offset)
                .map_err(|_e| DecideError::Component { source:
                    FinchBoardError::GpioLineReqError {
                        pos: "right".to_string(),
                         tech: "key".to_string()
                    }.into()})
                .unwrap()
                .request(LineRequestFlags::INPUT, &[0,0], "right_beam_breaks")
                .map_err(|_e| DecideError::Component { source:
                    FinchBoardError::GpioFlagReqError {
                        pos: "right".to_string(),
                        tech: "key".to_string(),
                        flag: "INPUT".to_string()}.into() })
                .unwrap();
                
            loop {
                match interrupt.next().await {
                    Some(event) => {
                        match event.unwrap().event_type() {
                            EventType::RisingEdge => {continue},
                            EventType::FallingEdge => {
                                let left_vals = left_handles.get_values().unwrap();
                                let right_vals = right_handles.get_values().unwrap();
                                tracing::info!("finchboard key interrupt - left key: {:?} - right key: {:?}", left_vals, right_vals);
                                let state = Self::State {
                                    peck_left: left_vals.iter().any(|&i|i==1),
                                    peck_right: right_vals.iter().any(|&i|i==1)
                                };
                                Self::send_state(&state, &sender).await;
                            }
                        }
                    }
                    None => {tracing::warn!("empty event from finchboard interrupt!?")}
                };
                let mut debounce = false;
                while !debounce {
                    tokio::select! {
                        _ = interrupt.next() => {}
                        _ = tokio::time::sleep(Duration::from_micros(20)) => {debounce=true}
                    }
                }
            }
        }));
        tracing::info!("finchboard peck keys initiated.");
    }

    fn change_state(&mut self, _state: Self::State) -> decide_protocol::Result<()> {
        tracing::error!("change state not implemented for finchboard peck keys.");
        Ok(())
    }

    fn set_parameters(&mut self, _params: Self::Params) -> decide_protocol::Result<()> {
        tracing::error!("change params not implemented for finchboard peck keys");
        Ok(())
    }

    fn get_state(&self) -> Self::State {
        Self::State{
            peck_left: false,
            peck_right: false,
        }
    }

    fn get_parameters(&self) -> Self::Params {
        Self::Params {}
    }

    async fn send_state(state: &Self::State, sender: &Sender<Any>) {
        tracing::debug!("emitting state change");
        sender.send(Any {
            type_url: String::from(Self::STATE_TYPE_URL),
            value: state.encode_to_vec(),
        }).await.map_err(|_e| DecideError::Component { source:
                FinchBoardError::SendError.into() })
            .unwrap();
    }

    async fn shutdown(&mut self) {
        if let Some(task_handle) = self.task_handle.take() {
            task_handle.abort();
            assert!(task_handle.await.unwrap_err().is_cancelled());
        }
    }
}

#[derive(Deserialize)]
pub struct FbKeyConfig {
    device_chip: String, // /dev/gpiochip4
    interrupt_chip: String, // /dev/gpiochip2
    interrupt_offset: u32, // 22-25
    left_offsets: Vec<u32>, // 12, 13
    right_offset: Vec<u32>, // 14, 15
}

pub mod proto {
    include!(concat!(env!("OUT_DIR"), "/_.rs"));
}

#[derive(Error, Debug)]
pub enum FinchBoardError {
    #[error("device not detected on i2c bus.")]
    MissingDevice,
    #[error("could not initialize gpio device {dev:?}")]
    GpioChipError{dev:String},
    #[error("could not request {pos:?} {tech:?} gpio lines")]
    GpioLineReqError{pos: String, tech: String},
    #[error("could not set {pos:?} {tech:?} to mode {flag:?}")]
    GpioFlagReqError{pos: String, tech: String, flag: String},
    #[error("could not set {pos:?} LED line to values {value:?}")]
    GpioLineSetError{pos: String, value: String},
    #[error("could not get value of {pos:?} {tech:?} gpio lines")]
    GpioLineGetError{pos: String, tech: String},
    #[error("could not get async handle for gpio line {line:?}")]
    GpioAsyncLineError{line: Vec<u8>},
    #[error("could not send state update")]
    SendError,
}