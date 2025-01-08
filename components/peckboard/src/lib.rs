use crate::PeckBoardError::{GpioAsyncLineError, GpioChipError, GpioFlagReqError, GpioLineGetError, GpioLineReqError, GpioLineSetError, SendError};
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
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{
    atomic::AtomicBool,
    Arc,
};
use std::time::Duration;
use thiserror::Error;
use tokio::sync::mpsc::Sender;
use tokio::{
    self, task::JoinHandle
};

pub struct PeckLeds {
    handles: MultiLineHandle,
    led_state: LedColor,
    state_sender: Sender<Any>,
}

pub struct PeckKeys {
    peck_left: Arc<AtomicBool>,
    peck_center: Arc<AtomicBool>,
    peck_right: Arc<AtomicBool>,
    state_sender: Sender<Any>,
    task_handle: Option<JoinHandle<()>>,
}

#[async_trait]
impl Component for PeckLeds {
    type State = proto::LedState;
    type Params = proto::LedParams;
    type Config = LedConfig;
    const STATE_TYPE_URL: &'static str = "type.googleapis.com/LedState";
    const PARAMS_TYPE_URL: &'static str =  "type.googleapis.com/LedParams";

    fn new(config: Self::Config, sender: Sender<Any>) -> Self {
        use std::fs;
        use std::path::{Path, PathBuf};
        use std::time::Duration;
        use std::thread;

        if !Path::new("/sys/class/i2c-adapter/i2c-1/1-0020").exists() {
            let chip_path = String::from("/sys/class/i2c-adapter/i2c-1/new_device");
            let sysfs_chip = fs::canonicalize(PathBuf::from(chip_path.clone()))
                .map_err(|_e| DecideError::Component { source:
                    PeckBoardError::InvalidFs { requested: chip_path}.into()})
                .unwrap();
            fs::write(sysfs_chip.clone(), "pcf8575 0x20")
                .map_err(|_e| DecideError::Component { source:
                    PeckBoardError::WriteError { path: sysfs_chip,
                                                 value: "pcf8575 0x20".to_string()}.into()})
                .unwrap();
            tracing::debug!("peckboard gpio chip initiated");
            assert!(Path::new("/sys/class/i2c-adapter/i2c-1/1-0020").exists());
        }
        thread::sleep(Duration::from_secs(2));
        let mut chip4 = Chip::new(config.peckboard_chip.clone())
            .map_err(|_e| DecideError::Component { source:
                PeckBoardError::GpioChipError {dev: config.peckboard_chip}.into()
            }).unwrap();
        let handles = chip4.get_lines(&config.led_offsets.clone())
            .map_err(|_e| DecideError::Component { source:
                GpioLineReqError {line: config.led_offsets.clone()}.into()
            }).unwrap()
            .request(LineRequestFlags::OUTPUT, &[0,0,0], "PeckLeds")
            .map_err(|_e| DecideError::Component { source:
                GpioFlagReqError {line: config.led_offsets.clone(), flag:"OUT".to_string()}.into()
            }).unwrap();
        PeckLeds {
            handles,
            led_state: LedColor::Off,
            state_sender: sender,
        }
    }

    async fn init(&mut self, _config: Self::Config) {
        tracing::info!("peck-led initiated")
    }

    fn change_state(&mut self, state: Self::State) -> decide_protocol::Result<()> {
        match state.led_state.as_str() {
            "off" => {self.led_state = LedColor::Off}
            "red" => {self.led_state = LedColor::Red}
            "blue" => {self.led_state = LedColor::Blue}
            "green" => {self.led_state = LedColor::Green}
            "white" => {self.led_state = LedColor::White}
            _ => {tracing::error!("peck-led state change contains invalid string {:?}", state.led_state.as_str());}
        }
        let lines_value = self.led_state.as_value();
        self.handles.set_values(&lines_value)
            .map_err(|_e| DecideError::Component { source:
                PeckBoardError::GpioLineSetError {value: Vec::from(lines_value) }.into()
            })?;
        let sender = self.state_sender.clone();
        tokio::spawn(async move {
            Self::send_state(&state, &sender).await;
        });
        Ok(())
    }

    fn set_parameters(&mut self, _params: Self::Params) -> decide_protocol::Result<()> {
        Ok(())
    }

    fn get_state(&self) -> Self::State {
        Self::State {
            led_state: match self.led_state {
                LedColor::Off => {String::from("off")}
                LedColor::Blue => {String::from("blue")}
                LedColor::Red => {String::from("red")}
                LedColor::Green => {String::from("green")}
                LedColor::White => {String::from("white")}
            }
        }
    }

    fn get_parameters(&self) -> Self::Params {
        Self::Params{}
    }

    async fn send_state(state: &Self::State, sender: &Sender<Any>) {
        tracing::debug!("emitting state change");
        Self::send_state(&state, &sender).await
    }

    async fn shutdown(&mut self) {
        self.handles.set_values(&LedColor::Off.as_value())
            .map_err(|_e| DecideError::Component { source:
                GpioLineSetError {value: Vec::from(&LedColor::Off.as_value()) }.into() })
            .unwrap();
    }
}

#[async_trait]
impl Component for PeckKeys {
    type State = proto::KeyState;
    type Params = proto::KeyParams;
    type Config = KeyConfig;
    const STATE_TYPE_URL: &'static str = "type.googleapis.com/KeyState";
    const PARAMS_TYPE_URL: &'static str = "type.googleapis.com/KeyParams";

    fn new(_config: Self::Config, sender: Sender<Any>) -> Self {
        PeckKeys {
            peck_left: Arc::new(AtomicBool::new(false)),
            peck_center:  Arc::new(AtomicBool::new(false)),
            peck_right:  Arc::new(AtomicBool::new(false)),
            state_sender: sender,
            task_handle: None,
        }
    }

    async fn init(&mut self, config: Self::Config) {
        let sender = self.state_sender.clone();

        self.task_handle = Some(tokio::spawn(async move {
            let mut chip2 = Chip::new(&config.interrupt_chip)
                .map_err(|_e| DecideError::Component { source:
                    GpioChipError { dev: config.interrupt_chip }.into()
                }).unwrap();
            let interrupt_offset = chip2.get_line(config.interrupt_offset.clone())
                .map_err(|_e| DecideError::Component { source:
                    GpioLineReqError {line: vec![config.interrupt_offset]}.into()
                }).unwrap();
            let mut interrupt = AsyncLineEventHandle::new(
                interrupt_offset.events(LineRequestFlags::INPUT,
                                        EventRequestFlags::BOTH_EDGES,      // we're interested in capturing FALLING_EDGE
                                        "Peckboard_Interrupt"     // but oddly setting flags to FALLING_EDGE still
                                        )             // gives us both edges.
                    .map_err(|_e| DecideError::Component {source:
                        GpioFlagReqError {line: vec![config.interrupt_offset],
                                          flag: "INPUT".to_string()}.into() })
                    .unwrap())
                .map_err(|_e| DecideError::Component { source:
                    GpioAsyncLineError {line: vec![config.interrupt_offset as u8]}.into()})
                .unwrap();

            while !Path::new("/sys/class/i2c-adapter/i2c-1/1-0020").exists() {
                tokio::time::sleep(Duration::from_secs(1)).await
            }
            let mut chip4 = Chip::new(&config.peckboard_chip)
                .map_err(|_e| DecideError::Component { source:
                GpioChipError { dev: config.peckboard_chip }.into()
                }).unwrap();
            chip4.get_lines(&config.ir_offsets)
                .map_err(|_e| DecideError::Component { source:
                    GpioLineReqError {line: config.ir_offsets.clone()}.into()})
                .unwrap()
                .request(LineRequestFlags::OUTPUT, &[1,1,1], "peckboard_ir")
                .map_err(|_e| DecideError::Component { source:
                    GpioFlagReqError {line: config.ir_offsets.clone(),
                                      flag: "OUTPUT".to_string()}.into() })
                .unwrap();
            let key_handles: MultiLineHandle = chip4.get_lines(&config.key_offsets)
                .map_err(|_e| DecideError::Component { source:
                    GpioLineReqError {line: config.key_offsets.clone()}.into() })
                .unwrap()
                .request(LineRequestFlags::INPUT, &[0,0,0], "peck_keys")
                .map_err(|_e| DecideError::Component { source:
                    GpioFlagReqError {line: config.key_offsets.clone(),
                                      flag: "INPUT".to_string()}.into() })
                .unwrap();

            loop {
                match interrupt.next().await {
                    Some(event) => {
                        match event.unwrap().event_type() {
                            EventType::FallingEdge => {
                                let values = key_handles.get_values()
                                    .map_err(|_e| DecideError::Component { source:
                                        GpioLineGetError.into() })
                                    .unwrap();
                                let first = values[0];
                                if values.iter().all(|&i| i == first) {
                                    continue
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
                            EventType::RisingEdge => { continue }
                        }
                    }
                    None => {tracing::error!("peck-key interrupted - no event?");continue},
                }
            }
        }));
        tracing::info!("peck-key initiated");
    }

    fn change_state(&mut self, state: Self::State) -> decide_protocol::Result<()> {
        self.peck_left.store(state.peck_left, Ordering::Release);
        self.peck_center.store(state.peck_right, Ordering::Release);
        self.peck_right.store(state.peck_center, Ordering::Release);

        let sender = self.state_sender.clone();
        tokio::spawn(async move {
            Self::send_state(&state, &sender).await;
        });
        Ok(())
    }

    fn set_parameters(&mut self, _params: Self::Params) -> decide_protocol::Result<()> {
        tracing::error!("PeckKeys set_params is empty. Make sure your script isn't using it without good reason");
        Ok(())
    }

    fn get_state(&self) -> Self::State {
        Self::State{
            peck_left: self.peck_left.load(Ordering::Acquire),
            peck_right: self.peck_right.load(Ordering::Acquire),
            peck_center: self.peck_center.load(Ordering::Acquire),
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
                SendError.into() })
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
pub struct LedConfig {
    peckboard_chip: String,
    led_offsets: Vec<u32>,
}
#[derive(Deserialize)]
pub struct KeyConfig {
    interrupt_chip: String,
    interrupt_offset: u32,
    peckboard_chip: String,
    key_offsets: Vec<u32>,
    ir_offsets: Vec<u32>,
}

pub mod proto {
    include!(concat!(env!("OUT_DIR"), "/_.rs"));
}

#[derive(Clone, Copy, Debug)]
pub enum LedColor {
    Off,
    Blue,
    Red,
    Green,
    White,
}
impl LedColor {
    //for light cycling
    fn _next(&mut self) -> &mut Self { //TODO: determine whether or not this method is necessary
        match self {
            LedColor::Off   => {*self = LedColor::Blue}
            LedColor::Blue   => {*self = LedColor::Red}
            LedColor::Red  => {*self = LedColor::Green}
            LedColor::Green => {*self = LedColor::White}
            LedColor::White   => {*self = LedColor::Off}
        };
        self
    }
    //convert LedColor to offset values
    fn as_value(&self) -> [u8; 3] {
        match self {
            LedColor::Off => {[0,0,0]}
            LedColor::Blue => {[1,0,0]}
            LedColor::Red => {[0,1,0]}
            LedColor::Green => {[0,0,1]}
            LedColor::White => {[1,1,1]}
        }
    }
}

#[derive(Error, Debug)]
pub enum PeckBoardError {
    #[error("could not find file for writing brightness value: {requested:?}")]
    InvalidFs{requested: String},
    #[error("could not write value {value:?} to file {path:?}")]
    WriteError{path: PathBuf, value: String},
    #[error("could not initialize gpio device {dev:?}")]
    GpioChipError{dev:String},
    #[error("could not request lines {line:?} from gpio device")]
    GpioLineReqError{line: Vec<u32>},
    #[error("could not set gpio lines {line:?} to mode {flag:?}")]
    GpioFlagReqError{line: Vec<u32>, flag: String},
    #[error("could not set gpio line to values {value:?}")]
    GpioLineSetError{value: Vec<u8>},
    #[error("could not get gpio line value")]
    GpioLineGetError,
    #[error("could not get async handle for gpio line {line:?}")]
    GpioAsyncLineError{line: Vec<u8>},
    #[error("could not send state update")]
    SendError,
}