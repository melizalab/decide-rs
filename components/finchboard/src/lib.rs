use async_trait::async_trait;
use futures::stream::StreamExt;
use decide_protocol::{error::DecideError,
                      Component};
use gpio_cdev::{AsyncLineEventHandle, Chip,
                EventRequestFlags,
                EventType,
                LineRequestFlags,
                LineHandle,
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

pub struct FbLeds {
    left_handle: MultiLineHandle,
    right_handle: MultiLineHandle,
    center_handle: LineHandle,
    left_led: LedColor,
    right_led: LedColor,
    center_led: LedColor,
    state_sender: Sender<Any>
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
            let right_handles: MultiLineHandle = dev_chip.get_lines(&config.right_offsets)
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

#[async_trait]
impl Component for FbLeds {
    type State = proto::FbLedState;
    type Params = proto::FbLedParams;
    type Config = FbLedConfig;
    const STATE_TYPE_URL: &'static str = "type.googleapis.com/FbLedState";
    const PARAMS_TYPE_URL: &'static str =  "type.googleapis.com/FbLedParams";

    fn new(config: Self::Config, sender: Sender<Any>) -> Self {
        if !Path::new("/sys/class/i2c-adapter/i2c-1/1-0020").exists() {
            panic!("{}", FinchBoardError::MissingDevice);
        };

        let mut dev_chip = Chip::new(&config.device_chip)
            .map_err(|_e| DecideError::Component { source:
                FinchBoardError::GpioChipError { dev: config.device_chip }.into()
            }).unwrap();
        
        let left_handle = dev_chip.get_lines(&config.left_offsets.clone())
            .map_err(|_e| DecideError::Component { source:
                FinchBoardError::GpioLineReqError {pos: "left".to_string(), tech: "led".to_string()}.into()
            }).unwrap()
            .request(LineRequestFlags::OUTPUT, &LedColor::Off.as_value(), "fb_led_left")
            .map_err(|_e| DecideError::Component { source:
                FinchBoardError::GpioFlagReqError {pos: "left".to_string(), tech: "led".to_string(), flag:"OUT".to_string()}.into()
            }).unwrap();
        let right_handle = dev_chip.get_lines(&config.right_offsets.clone())
            .map_err(|_e| DecideError::Component { source:
                FinchBoardError::GpioLineReqError {pos: "right".to_string(), tech: "led".to_string()}.into()
            }).unwrap()
            .request(LineRequestFlags::OUTPUT, &LedColor::Off.as_value(), "fb_led_right")
            .map_err(|_e| DecideError::Component { source:
                FinchBoardError::GpioFlagReqError {pos: "right".to_string(), tech: "led".to_string(), flag:"OUT".to_string()}.into()
            }).unwrap();
        let center_handle = dev_chip.get_line(config.center_offset.clone())
            .map_err(|_e| DecideError::Component { source:
                FinchBoardError::GpioLineReqError {pos: "center".to_string(), tech: "led".to_string()}.into()
            }).unwrap()
            .request(LineRequestFlags::OUTPUT, LedColor::Off.as_mono_value(), "fb_led_center")
            .map_err(|_e| DecideError::Component { source:
                FinchBoardError::GpioFlagReqError {pos: "center".to_string(), tech: "led".to_string(), flag:"OUT".to_string()}.into()
            }).unwrap();

        FbLeds {
            left_handle: left_handle,
            right_handle: right_handle,
            center_handle: center_handle,
            left_led: LedColor::Off,
            right_led: LedColor::Off,
            center_led: LedColor::Off,
            state_sender: sender,
        }
    }

    async fn init(&mut self, _config: Self::Config) {
        tracing::info!("finchboard leds initiated");
    }

    fn change_state(&mut self, state: Self::State) -> decide_protocol::Result<()> {

        if LedColor::from_str(&state.left_led) != self.left_led {
            self.left_handle.set_values(&LedColor::val_from_str(&state.left_led.clone()))
            .map_err(|_e| DecideError::Component { source:
                FinchBoardError::GpioLineSetError {pos: "left".to_string(), value: state.left_led.clone()}.into()
            })?;
            self.left_led = LedColor::from_str(&state.left_led.clone())
        };

        if LedColor::from_str(&state.right_led) != self.right_led {
            self.right_handle.set_values(&LedColor::val_from_str(&state.right_led.clone()))
            .map_err(|_e| DecideError::Component { source:
                FinchBoardError::GpioLineSetError {pos: "right".to_string(), value: state.right_led.clone()}.into()
            })?;
            self.right_led = LedColor::from_str(&state.right_led.clone())
        };

        if LedColor::from_str(&state.center_led) != self.center_led {
            self.center_handle.set_value(LedColor::mono_val_from_str(&state.center_led))
            .map_err(|_e| DecideError::Component { source:
                FinchBoardError::GpioLineSetError {pos: "center".to_string(), value: state.center_led.clone()}.into()
            })?;
            self.center_led = LedColor::from_str(&state.center_led.clone())
        };

        let sender = self.state_sender.clone();
        tokio::spawn(async move {
            Self::send_state(&state, &sender).await;
        });
        Ok(())
    }

    fn set_parameters(&mut self, _params: Self::Params) -> decide_protocol::Result<()> {
        tracing::error!("change params not implemented for finchboard leds");
        Ok(())
    }

    fn get_state(&self) -> Self::State {
        Self::State {
            left_led: self.left_led.to_str(),
            right_led: self.right_led.to_str(),
            center_led: self.center_led.to_str()
        }
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

    fn get_parameters(&self) -> Self::Params {
        Self::Params{}
    }

    async fn shutdown(&mut self) {
        self.left_handle.set_values(&LedColor::Off.as_value())
        .map_err(|_e| DecideError::Component { source:
            FinchBoardError::GpioLineSetError { pos: "left".to_string(), value: LedColor::Off.to_str()}.into()
        }).unwrap();
        self.right_handle.set_values(&LedColor::Off.as_value())
        .map_err(|_e| DecideError::Component { source:
            FinchBoardError::GpioLineSetError { pos: "right".to_string(), value: LedColor::Off.to_str()}.into()
        }).unwrap();
        self.center_handle.set_value(LedColor::Off.as_mono_value())
        .map_err(|_e| DecideError::Component { source:
            FinchBoardError::GpioLineSetError { pos: "center".to_string(), value: LedColor::Off.to_str()}.into()
        }).unwrap();

    }
}

#[derive(Deserialize)]
pub struct FbKeyConfig {
    device_chip: String, // /dev/gpiochip4
    interrupt_chip: String, // /dev/gpiochip2
    interrupt_offset: u32, // 22-25
    left_offsets: Vec<u32>, // 12, 13
    right_offsets: Vec<u32>, // 14, 15
}
#[derive(Deserialize)]
pub struct FbLedConfig {
    device_chip: String, // /dev/gpiochip4
    left_offsets: Vec<u32>, // 0, 3, 6
    center_offset:u32, // 8
    right_offsets: Vec<u32> // 1,4,7
}

pub mod proto {
    include!(concat!(env!("OUT_DIR"), "/_.rs"));
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LedColor {
    Off,
    Blue,
    Red,
    Green,
    White
}
impl LedColor {
    fn as_value(&self) -> [u8; 3] {
        match self {
            LedColor::Off => {[0,0,0]}
            LedColor::Blue => {[1,0,0]}
            LedColor::Red => {[0,1,0]}
            LedColor::Green => {[0,0,1]}
            LedColor::White => {[1,1,1]}
        }
    }
    fn as_mono_value(&self) -> u8 {
        match self {
            LedColor::Off => 0,
            LedColor::Red => 1,
            _ => 0,
        }
    }
    fn to_str(&self) -> String {
        match self {
            LedColor::Off => {"Off".to_string()}
            LedColor::Red => {"Red".to_string()}
            LedColor::Blue => {"Blue".to_string()}
            LedColor::Green => {"Green".to_string()}
            LedColor::White => {"White".to_string()}
        }
    }
    fn from_str(text: &str) -> Self {
        match text {
            "Off" => LedColor::Off,
            "Red" => LedColor::Red,
            "Blue" => LedColor::Blue,
            "Green" => LedColor::Green,
            "White" => LedColor::White,
            _ => LedColor::Off
        }
    }
    fn val_from_str(value: &str) -> [u8; 3] {
        match value {
            "Off" => {[0,0,0]}
            "Red" => {[0,1,0]}
            "Green" => {[0,0,1]}
            "Blue" => {[1,0,0]}
            "White" => {[1,1,1]}
            _ => {[0,0,0]}
        }
    }
    fn mono_val_from_str(value: &str) -> u8 {
        match value {
            "Off" => 0,
            "Red" => 1,
            _ => 0
        }
    }
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
