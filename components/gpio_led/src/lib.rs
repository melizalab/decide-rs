use async_trait::async_trait;
use decide_protocol::{error::DecideError,
                      Component};
use gpio_cdev::{Chip,
                LineHandle,
                LineRequestFlags,
};
use prost::Message;
use prost_types::Any;
use serde::Deserialize;
use thiserror::Error;
use tokio::sync::mpsc::Sender;
use tokio;

pub struct GpioLed {
    handle: LineHandle,
    led_state: bool,
    state_sender: Sender<Any>,
}

#[derive(Deserialize)]
pub struct LedConfig {
    chip: String,
    line: u32,
}

pub mod proto {
    include!(concat!(env!("OUT_DIR"), "/_.rs"));
}

#[async_trait]
impl Component for GpioLed {
    type State = proto::GledState;
    type Params = proto::GledParams;
    type Config = LedConfig;
    const STATE_TYPE_URL: &'static str = "type.googleapis.com/LedState";
    const PARAMS_TYPE_URL: &'static str =  "type.googleapis.com/LedParams";

    fn new(config: Self::Config, sender: Sender<Any>) -> Self {

        let mut chip = Chip::new(config.chip.clone())
            .map_err(|_e| DecideError::Component { source:
                LedError::GpioChipError {dev: config.chip}.into()
            }).unwrap();
        let lineval:u32 = config.line - 64;
        let handle = chip.get_line(lineval.clone())
            .map_err(|_e| DecideError::Component { source:
                LedError::GpioLineReqError {line: lineval}.into()
            }).unwrap()
            .request(LineRequestFlags::OUTPUT, 0, "PeckLeds")
            .map_err(|_e| DecideError::Component { source:
                LedError::GpioFlagReqError {line: lineval, flag:"OUT".to_string()}.into()
            }).unwrap();
        GpioLed {
            handle,
            led_state: false,
            state_sender: sender,
        }
    }

    async fn init(&mut self, _config: Self::Config) {
        tracing::info!("led initiated")
    }

    fn change_state(&mut self, state: Self::State) -> decide_protocol::Result<()> {
        let line_value = if state.switch {1} else {0};
        self.led_state = state.switch;
        self.handle.set_value(line_value)
            .map_err(|_e| DecideError::Component { source:
                LedError::GpioLineSetError {value: line_value }.into()
            })?;
        let sender = self.state_sender.clone();
        futures::executor::block_on(Self::send_state(&state, &sender));
        Ok(())
    }

    fn set_parameters(&mut self, _params: Self::Params) -> decide_protocol::Result<()> {
        Ok(())
    }

    fn get_state(&self) -> Self::State {
        Self::State {
            switch: self.led_state 
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
        }).await.map_err(|_e| DecideError::Component { source:
        LedError::SendError.into() }).unwrap();
    }

    async fn shutdown(&mut self) {
        self.handle.set_value(0)
            .map_err(|_e| DecideError::Component { source:
                LedError::GpioLineSetError {value: 0}.into() })
            .unwrap();
    }
}


#[derive(Error, Debug)]
pub enum LedError {
    #[error("could not initialize gpio device {dev:?}")]
    GpioChipError{dev:String},
    #[error("could not request lines {line:?} from gpio device")]
    GpioLineReqError{line: u32},
    #[error("could not set gpio lines {line:?} to mode {flag:?}")]
    GpioFlagReqError{line: u32, flag: String},
    #[error("could not set gpio line to values {value:?}")]
    GpioLineSetError{value: u8},
    #[error("could not send state update")]
    SendError,
}