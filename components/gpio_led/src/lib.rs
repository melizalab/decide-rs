use async_trait::async_trait;
use decide_protocol::{error::DecideError,
                      Component};
use gpio_cdev::{Chip,
                LineHandle,
                MultiLineHandle,
                LineRequestFlags,
};
use prost::Message;
use prost_types::Any;
use serde::Deserialize;
use thiserror::Error;
use tokio::sync::mpsc::Sender;
use tokio;

pub struct MonoLed {
    handle: LineHandle,
    led_state: LedColor,
    state_sender: Sender<Any>
}
pub struct RGBLed {
    handle: MultiLineHandle,
    led_state: LedColor,
    state_sender: Sender<Any>
}

#[derive(Deserialize)]
pub struct RGBLedConfig {
    gpio_chip: String,
    gpio_lines: [u32; 3],
}
#[derive(Deserialize)]
pub struct MonoLedConfig {
    gpio_chip: String,
    gpio_line: u32,
}

pub mod proto {
    include!(concat!(env!("OUT_DIR"), "/_.rs"));
}

#[async_trait]
impl Component for MonoLed {
    type State = proto::LedState;
    type Params = proto::LedParams;
    type Config = MonoLedConfig;
    const STATE_TYPE_URL: &'static str = "type.googleapis.com/LedState";
    const PARAMS_TYPE_URL: &'static str =  "type.googleapis.com/LedParams";

    fn new(config: Self::Config, sender: Sender<Any>) -> Self {

        let mut dev_chip = Chip::new(&config.gpio_chip)
            .map_err(|_e| DecideError::Component { source:
                LedError::GpioChipError { dev: config.gpio_chip.clone() }.into()
            }).unwrap();

        let handle = dev_chip.get_line(config.gpio_line)
            .map_err(|_e| DecideError::Component { source:
                LedError::GpioLineReqError {
                    line:config.gpio_line,
                    dev:config.gpio_chip.clone()
                }.into()
            }).unwrap()
            .request(LineRequestFlags::OUTPUT, LedColor::Off.mono_as_value(), "gpio_mono_led")
            .map_err(|_e| DecideError::Component { source:
                LedError::GpioFlagReqError {
                    line:config.gpio_line,
                    dev:config.gpio_chip.clone(),
                    flag:"OUT".to_string()
                }.into()
            }).unwrap();
        MonoLed {
            handle,
            led_state: LedColor::Off,
            state_sender: sender,
        }
    }

    async fn init(&mut self, _config: Self::Config) {
        tracing::info!("mono led initiated")
    }

    fn change_state(&mut self, state: Self::State) -> decide_protocol::Result<()> {
        self.led_state = LedColor::from_str(&state.state);
        self.handle.set_value(self.led_state.mono_as_value())
            .map_err(|_e| DecideError::Component { source:
                LedError::GpioLineSetError {
                    line: self.handle.line().offset(),
                    value: self.led_state.mono_as_value()
                }.into()
            })?;
        let sender = self.state_sender.clone();
        futures::executor::block_on(Self::send_state(&state, &sender));
        Ok(())
    }

    fn set_parameters(&mut self, _params: Self::Params) -> decide_protocol::Result<()> {
        tracing::error!("change params not implemented for led");
        Ok(())
    }

    fn get_state(&self) -> Self::State {
        Self::State {
            state: self.led_state.to_str() 
        }
    }

    fn get_parameters(&self) -> Self::Params {
        Self::Params{}
    }

    async fn send_state(state: &Self::State, sender: &Sender<Any>) {
        tracing::debug!("Emiting state change");
        sender.send(Any {
            type_url: String::from(Self::STATE_TYPE_URL),
            value: state.encode_to_vec(),
        }).await.map_err(|_e| DecideError::Component {
            source: LedError::SendError.into() 
        }).unwrap();
    }

    async fn shutdown(&mut self) {
        self.handle.set_value(0)
            .map_err(|_e| DecideError::Component {
                source: LedError::GpioLineSetError {
                    line: self.handle.line().offset(),
                    value: self.led_state.mono_as_value()
                }.into()
            }).unwrap()
    }
}

#[async_trait]
impl Component for RGBLed {
    type State = proto::LedState;
    type Params = proto::LedParams;
    type Config = RGBLedConfig;
    const STATE_TYPE_URL: &'static str = "type.googleapis.com/LedState";
    const PARAMS_TYPE_URL: &'static str =  "type.googleapis.com/LedParams";

    fn new(config: Self::Config, sender: Sender<Any>) -> Self {

        let mut dev_chip = Chip::new(&config.gpio_chip)
            .map_err(|_e| DecideError::Component { source:
                LedError::GpioChipError { dev: config.gpio_chip.clone() }.into()
            }).unwrap();

        let handle = dev_chip.get_lines(&config.gpio_lines)
            .map_err(|_e| DecideError::Component { source:
                LedError::GpioLinesReqError {
                    lines: config.gpio_lines,
                    dev:config.gpio_chip.clone()
                }.into()
            }).unwrap()
            .request(LineRequestFlags::OUTPUT, &LedColor::Off.as_value(), "gpio_rgb_led")
            .map_err(|_e| DecideError::Component { source:
                LedError::GpioFlagsReqError {
                    lines:config.gpio_lines,
                    dev:config.gpio_chip.clone(),
                    flag:"OUT".to_string()
                }.into()
            }).unwrap();
        RGBLed {
            handle,
            led_state: LedColor::Off,
            state_sender: sender,
        }
    }

    async fn init(&mut self, _config: Self::Config) {
        tracing::info!("mono led initiated")
    }

    fn change_state(&mut self, state: Self::State) -> decide_protocol::Result<()> {
        self.led_state = LedColor::from_str(&state.state);
        self.handle.set_values(&self.led_state.as_value())
            .map_err(|_e| DecideError::Component { source:
                LedError::GpioLinesSetError {
                    value: self.led_state.as_value()
                }.into()
            }).unwrap();
        let sender = self.state_sender.clone();
        futures::executor::block_on(Self::send_state(&state, &sender));
        Ok(())
    }

    fn set_parameters(&mut self, _params: Self::Params) -> decide_protocol::Result<()> {
        tracing::error!("change params not implemented for led");
        Ok(())
    }

    fn get_state(&self) -> Self::State {
        Self::State {
            state: self.led_state.to_str() 
        }
    }

    fn get_parameters(&self) -> Self::Params {
        Self::Params{}
    }

    async fn send_state(state: &Self::State, sender: &Sender<Any>) {
        tracing::debug!("Emiting state change");
        sender.send(Any {
            type_url: String::from(Self::STATE_TYPE_URL),
            value: state.encode_to_vec(),
        }).await.map_err(|_e| DecideError::Component {
            source: LedError::SendError.into() 
        }).unwrap();
    }

    async fn shutdown(&mut self) {
        self.handle.set_values(&LedColor::Off.as_value())
            .map_err(|_e| DecideError::Component {
                source: LedError::GpioLinesSetError {
                    value: LedColor::Off.as_value()
                }.into()
            }).unwrap()
    }
}


#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LedColor {
    Off,
    Blue,
    Red,
    Green,
    White,
    On
}
impl LedColor {
    fn as_value(&self) -> [u8; 3] {
        match self {
            LedColor::Off => {[0,0,0]}
            LedColor::Blue => {[1,0,0]}
            LedColor::Red => {[0,1,0]}
            LedColor::Green => {[0,0,1]}
            LedColor::White => {[1,1,1]}
            LedColor::On => {[1,1,1]}
        }
    }
    fn mono_as_value(&self) -> u8 {
        match self {
            LedColor::Off => 0,
            LedColor::On => 1,
            _ => 1
        }
    }
    fn to_str(&self) -> String {
        match self {
            LedColor::Off => {"off".to_string()}
            LedColor::Red => {"red".to_string()}
            LedColor::Blue => {"blue".to_string()}
            LedColor::Green => {"green".to_string()}
            LedColor::White => {"white".to_string()}
            LedColor::On => {"on".to_string()}
        }
    }
    fn from_str(text: &str) -> Self {
        match text {
            "off" => LedColor::Off,
            "red" => LedColor::Red,
            "blue" => LedColor::Blue,
            "green" => LedColor::Green,
            "white" => LedColor::White,
            "on" => LedColor::On,
            _ => LedColor::Off
        }
    }
}

#[derive(Error, Debug)]
pub enum LedError {
    #[error("could not request gpio device {dev:?}.")]
    GpioChipError{dev:String},
    #[error("could not request gpio line {line:?} from {dev:?}.")]
    GpioLineReqError{line: u32, dev: String},
    #[error("could not set line {line:?} from {dev:?} to mode {flag:?}")]
    GpioFlagReqError{line: u32, dev: String, flag: String},
    #[error("could not set LED line {line:?} to value {value:?}")]
    GpioLineSetError{line: u32, value: u8},
    #[error("could not request gpio lines {lines:?} from {dev:?}.")]
    GpioLinesReqError{lines: [u32;3], dev: String},
    #[error("could not set lines {lines:?} from {dev:?} to mode {flag:?}")]
    GpioFlagsReqError{lines: [u32;3], dev: String, flag: String},
    #[error("could not set LED lines to value {value:?}")]
    GpioLinesSetError{value: [u8;3]},
    #[error("could not send state update")]
    SendError,
}
