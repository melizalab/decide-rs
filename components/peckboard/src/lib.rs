use async_trait::async_trait;
use decide_protocol::{error::DecideError,
                      Component};
use futures::stream::StreamExt;
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
use tokio::{self, sync::mpsc::Sender, task::JoinHandle, time::Duration};

pub struct PeckboardKeys {
    state_sender: Sender<Any>,
    task_handle: Option<JoinHandle<()>>,
}

pub struct FinchboardKeys {
    state_sender: Sender<Any>,
    task_handle: Option<JoinHandle<()>>,
}


#[async_trait]
impl Component for PeckboardKeys {
    type State = proto::KeyState;
    type Params = proto::KeyParams;
    type Config = PeckboardConfig;
    const STATE_TYPE_URL: &'static str = "type.googleapis.com/KeyState";
    const PARAMS_TYPE_URL: &'static str = "type.googleapis.com/KeyParams";

    fn new(_config: Self::Config, sender: Sender<Any>) -> Self {
        if !Path::new("/sys/class/i2c-adapter/i2c-1/1-0020").exists() {
            panic!("{}", PeckError::MissingDevice);
        };
        PeckboardKeys {
            state_sender: sender,
            task_handle: None,
        }
    }

    async fn init(&mut self, config: Self::Config) {
        let sender = self.state_sender.clone();

        self.task_handle = Some(tokio::spawn(async move {
            let mut chip2 = Chip::new(&config.interrupt_chip)
                .map_err(|_e| DecideError::Component { source:
                    PeckError::GpioChipError { dev: config.interrupt_chip.clone() }.into()
                }).unwrap();
            let interrupt_offset = chip2.get_line(config.interrupt_offset.clone())
                .map_err(|_e| DecideError::Component { source:
                    PeckError::GpioLineReqError {
                        lines: vec![config.interrupt_offset],
                        dev: config.interrupt_chip.clone()
                    }.into()
                }).unwrap();
            let mut interrupt = AsyncLineEventHandle::new(
                interrupt_offset.events(LineRequestFlags::INPUT,
                                        EventRequestFlags::BOTH_EDGES,      // we're interested in capturing FALLING_EDGE
                                        "peck-key-interrupt"     // but oddly setting flags to FALLING_EDGE still
                                        )             // gives us both edges.
                    .map_err(|_e| DecideError::Component {source:
                        PeckError::GpioFlagReqError {lines: vec![config.interrupt_offset],
                                          flag: "INPUT".to_string()}.into() })
                    .unwrap())
                .map_err(|_e| DecideError::Component { source:
                    PeckError::GpioAsyncLineError {line: config.interrupt_offset}.into()})
                .unwrap();

            let mut chip4 = Chip::new(&config.device_chip)
                .map_err(|_e| DecideError::Component { source:
                    PeckError::GpioChipError { dev: config.device_chip.clone() }.into()
                }).unwrap();
            chip4.get_lines(&config.ir_offsets)
                .map_err(|_e| DecideError::Component { source:
                    PeckError::GpioLineReqError {
                        lines: config.ir_offsets.clone(),
                        dev: config.device_chip.clone()
                    }.into()})
                .unwrap()
                .request(LineRequestFlags::OUTPUT, &[1,1,1], "peck-key-ir")
                .map_err(|_e| DecideError::Component { source:
                    PeckError::GpioFlagReqError {
                        lines: config.ir_offsets.clone(),
                        flag: "OUTPUT".to_string()}.into() })
                .unwrap();
            let key_handles: MultiLineHandle = chip4.get_lines(&config.key_offsets)
                .map_err(|_e| DecideError::Component { source:
                    PeckError::GpioLineReqError {
                        lines: config.key_offsets.clone(),
                        dev: config.device_chip.clone()
                    }.into() })
                .unwrap()
                .request(LineRequestFlags::INPUT, &[0,0,0], "peck-keys")
                .map_err(|_e| DecideError::Component { source:
                    PeckError::GpioFlagReqError {
                        lines: config.key_offsets.clone(),
                        flag: "INPUT".to_string()}.into() })
                .unwrap();

            'poll: loop {
                match interrupt.next().await {
                    Some(event) => {
                        let evt = event.unwrap().event_type();
                        if (evt==EventType::FallingEdge)|(evt==EventType::RisingEdge) {
                            let values = key_handles.get_values()
                                .map_err(|_e: gpio_cdev::Error| DecideError::Component { source:
                                    PeckError::GpioLineGetError.into() })
                                .unwrap();
                            if values.iter().all(|&i| i == 0) {
                                continue 'poll
                            } else {
                                tracing::info!("peck-key interrupted - event {:?} registered", values);
                                let state = Self::State {
                                    peck_left: values[2] != 0,
                                    peck_center: values[1] != 0,
                                    peck_right: values[0] != 0,
                                };
                                Self::send_state(&state, &sender).await;
                            }
                        }
                    }
                    None => {tracing::error!("peck-key interrupted - no event?"); continue 'poll},
                };
                let mut debounce = false;
                while !debounce {
                    tokio::select! {
                        _ = interrupt.next() => {}
                        _ = tokio::time::sleep(Duration::from_micros(20)) => {debounce=true}
                    }}
            }
        }));
        tracing::info!("peck-key initiated");
    }

    fn change_state(&mut self, _state: Self::State) -> decide_protocol::Result<()> {
        tracing::error!("change state not implemented for peckboard keys.");
        Ok(())

    }

    fn set_parameters(&mut self, _params: Self::Params) -> decide_protocol::Result<()> {
        tracing::error!("PeckKeys set_params is empty. Make sure your script isn't using it without good reason");
        Ok(())
    }

    fn get_state(&self) -> Self::State {
        Self::State{
            peck_left: false,
            peck_center: false,
            peck_right: false
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
                PeckError::SendError.into() })
            .unwrap();
    }

    async fn shutdown(&mut self) {
        if let Some(task_handle) = self.task_handle.take() {
            task_handle.abort();
            assert!(task_handle.await.unwrap_err().is_cancelled());
        }
    }
}

#[async_trait]
impl Component for FinchboardKeys {
    type State = proto::KeyState;
    type Params = proto::KeyParams;
    type Config = FinchboardConfig;
    const STATE_TYPE_URL: &'static str = "type.googleapis.com/KeyState";
    const PARAMS_TYPE_URL: &'static str = "type.googleapis.com/KeyParams";

    fn new(_config: Self::Config, sender: Sender<Any>) -> Self {

        if !Path::new("/sys/class/i2c-adapter/i2c-1/1-0020").exists() {
            panic!("{}", PeckError::MissingDevice);
        };

        FinchboardKeys {
            state_sender: sender,
            task_handle: None,
        }
    }

    async fn init(&mut self, config: Self::Config) {

        let sender = self.state_sender.clone();

        self.task_handle = Some(tokio::spawn( async move {
            let mut int_chip = Chip::new(&config.interrupt_chip)
                .map_err(|_e| DecideError::Component { source: 
                    PeckError::GpioChipError {
                        dev: config.interrupt_chip.clone()}.into()
                }).unwrap();
            let int_line  = int_chip.get_line(config.interrupt_offset.clone())
                .map_err(|_e| DecideError::Component { source:
                    PeckError::GpioLineReqError {
                        dev: config.interrupt_chip.clone(),
                        lines: vec![config.interrupt_offset]
                    }.into()
                }).unwrap();
            let mut interrupt = AsyncLineEventHandle::new(
                int_line.events(LineRequestFlags::INPUT,
                                        EventRequestFlags::BOTH_EDGES,
                                        "finchboard interrupt")
                    .map_err(|_e| DecideError::Component { source: 
                        PeckError::GpioFlagReqError {
                            lines: vec![config.interrupt_offset],
                            flag: "INPUT".to_string()}.into()    
                    }).unwrap()
            ).map_err(|_e| DecideError::Component { source: 
                PeckError::GpioAsyncLineError { line: config.interrupt_offset}.into()
            }).unwrap(); 

            let mut dev_chip = Chip::new(&config.device_chip)
                .map_err(|_e| DecideError::Component { source:
                    PeckError::GpioChipError { dev: config.device_chip.clone() }.into()
                }).unwrap();
            let left_handles: MultiLineHandle = dev_chip.get_lines(&config.left_offsets)
                .map_err(|_e| DecideError::Component { source:
                    PeckError::GpioLineReqError {
                        dev: config.device_chip.clone(),
                        lines: config.left_offsets.clone()
                    }.into() })
                .unwrap()
                .request(LineRequestFlags::INPUT, &[0,0], "left_beam_breaks")
                .map_err(|_e| DecideError::Component { source:
                    PeckError::GpioFlagReqError {
                        lines: config.left_offsets.clone(),
                        flag: "INPUT".to_string()}.into()
                }).unwrap();
            let right_handles: MultiLineHandle = dev_chip.get_lines(&config.right_offsets)
                .map_err(|_e| DecideError::Component { source:
                    PeckError::GpioLineReqError {
                        dev: config.device_chip.clone(),
                        lines: config.right_offsets.clone()
                    }.into() })
                .unwrap()
                .request(LineRequestFlags::INPUT, &[0,0], "right_beam_breaks")
                .map_err(|_e| DecideError::Component { source:
                    PeckError::GpioFlagReqError {
                        lines: config.right_offsets.clone(),
                        flag: "INPUT".to_string()}.into()
                }).unwrap();
                
            loop {
                match interrupt.next().await {
                    Some(event) => {
                        match event.unwrap().event_type() {
                            EventType::RisingEdge => {continue},
                            EventType::FallingEdge => {
                                let left_vals = left_handles.get_values()                                
                                    .map_err(|_e: gpio_cdev::Error| DecideError::Component { source:
                                        PeckError::GpioLineGetError.into() 
                                    }).unwrap();
                                let right_vals = right_handles.get_values()
                                    .map_err(|_e: gpio_cdev::Error| DecideError::Component { source:
                                        PeckError::GpioLineGetError.into() 
                                    }).unwrap();

                                tracing::info!("finchboard key interrupt - left key: {:?} - right key: {:?}", left_vals, right_vals);
                                let state = Self::State {
                                    peck_left: left_vals.iter().any(|&i|i==1),
                                    peck_center: false,
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
            peck_center: false,
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
                PeckError::SendError.into() })
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
pub struct PeckboardConfig {
    interrupt_chip: String,
    interrupt_offset: u32,
    device_chip: String,
    key_offsets: Vec<u32>,
    ir_offsets: Vec<u32>,
}

#[derive(Deserialize)]
pub struct FinchboardConfig {
    interrupt_chip: String,
    interrupt_offset: u32,
    device_chip: String,
    left_offsets: Vec<u32>,
    right_offsets: Vec<u32>
}

pub mod proto {
    include!(concat!(env!("OUT_DIR"), "/_.rs"));
}


#[derive(Error, Debug)]
pub enum PeckError {
    #[error("device not detected on i2c bus.")]
    MissingDevice,
    #[error("could not initialize gpio device {dev:?}")]
    GpioChipError{dev:String},

    #[error("could not request gpio lines {lines:?} on {dev:?}")]
    GpioLineReqError{lines: Vec<u32>, dev: String},
    #[error("could not set lines {lines:?} to mode {flag:?}")]
    GpioFlagReqError{lines: Vec<u32>, flag: String},
    #[error("could not get value of peck lines.")]
    GpioLineGetError,
    #[error("could not get async handle for gpio line {line:?}")]
    GpioAsyncLineError{line: u32},
    #[error("could not send state update")]
    SendError,
}