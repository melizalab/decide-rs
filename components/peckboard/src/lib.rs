use gpio_cdev::{Chip, AsyncLineEventHandle,
                LineRequestFlags,
                MultiLineHandle,
                EventRequestFlags,
                errors::Error as GpioError
};
use futures::stream::StreamExt;
use tokio::sync::mpsc::Sender;
use async_trait::async_trait;
use decide_proto::{Component,
                   error::{ComponentError, DecideError}
};
use prost::Message;
use prost_types::Any;
use serde::Deserialize;
use std::sync::{
    atomic::{AtomicBool,// Ordering
    },
    Arc,
};
use std::sync::atomic::Ordering;
use tokio::{
    self, sync::mpsc,
};

pub mod peckboard {
    include!(concat!(env!("OUT_DIR"), "/_.rs"));
}

pub struct PeckLeds {
    handles: MultiLineHandle,
    led_state: LedColor,
    state_sender: Option<mpsc::Sender<Any>>,
}

pub struct PeckKeys {
    peck_left: Arc<AtomicBool>,
    peck_center: Arc<AtomicBool>,
    peck_right: Arc<AtomicBool>,
    state_sender: Option<mpsc::Sender<Any>>,
}

#[async_trait]
impl Component for PeckLeds {
    type State = peckboard::LedState;
    type Params = peckboard::LedParams;
    type Config = Config;
    const STATE_TYPE_URL: &'static str = "melizalab.org/proto/led_state";
    const PARAMS_TYPE_URL: &'static str =  "melizalab.org/proto/led_params";

    fn new(config: Self::Config) -> Self {
        let mut chip4 = Chip::new(config.peckboard_chip.clone())
            .map_err(|e:GpioError| ComponentError::ChipError {source:e, chip: config.peckboard_chip.clone()}).unwrap();
        let handles = chip4.get_lines(&config.led_offsets.clone())
            .map_err(|e:GpioError| ComponentError::LinesGetError {source:e, lines: config.led_offsets.clone()}).unwrap()
            .request(LineRequestFlags::OUTPUT, &[], "PeckLeds")
            .map_err(|e:GpioError| ComponentError::LinesReqError {source:e, lines: config.led_offsets.clone()}).unwrap();
        PeckLeds {
            handles,
            led_state: LedColor::Off,
            state_sender: None,
        }
    }

    async fn init(&mut self, _config: Self::Config, sender: Sender<Any>) {
        self.state_sender = Some(sender.clone());
    }

    fn change_state(&mut self, state: Self::State) -> decide_proto::Result<()> {
        match state.led_state.as_str() {
            "off" => {self.led_state = LedColor::Off}
            "red" => {self.led_state = LedColor::Red}
            "blue" => {self.led_state = LedColor::Blue}
            "green" => {self.led_state = LedColor::Green}
            "white" => {self.led_state = LedColor::White}
            _ => {tracing::error!("Pecklight state received invalid");}
        }
        let lines_value = self.led_state.as_value();
        self.handles.set_values(&lines_value)
            .map_err(|e:GpioError| ComponentError::LinesSetError {source:e}).unwrap();
        let sender = self.state_sender.as_mut().cloned().unwrap();
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

    fn set_parameters(&mut self, _params: Self::Params) -> decide_proto::Result<()> {
        todo!()
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
}

#[async_trait]
impl Component for PeckKeys {
    type State = peckboard::KeyState;
    type Params = peckboard::KeyParams;
    type Config = Config;
    const STATE_TYPE_URL: &'static str = ""; //TODO: Add peckboard links
    const PARAMS_TYPE_URL: &'static str = "";

    fn new(_config: Self::Config) -> Self {
        PeckKeys {
            peck_left: Arc::new(AtomicBool::new(false)),
            peck_center:  Arc::new(AtomicBool::new(false)),
            peck_right:  Arc::new(AtomicBool::new(false)),
            state_sender: None,
        }
    }

    async fn init(&mut self, config: Self::Config, sender: Sender<Any>) {
        self.state_sender = Some(sender.clone());
        tokio::spawn(async move {
            let mut chip2 = Chip::new(&config.interrupt_chip)
                .map_err(|e:GpioError| ComponentError::ChipError {source:e, chip: config.interrupt_chip.clone()}).unwrap();
            let interrupt_offset = chip2.get_line(config.interrupt_offset.clone())
                .map_err(|e:GpioError| ComponentError::LineGetError {source:e, line: config.interrupt_offset.clone()}).unwrap();
            let mut interrupt = AsyncLineEventHandle::new(interrupt_offset.events(
                LineRequestFlags::INPUT,
                EventRequestFlags::BOTH_EDGES,
                "Peckboard Interrupt"
            ).unwrap())
                .map_err(|e:GpioError| ComponentError::AsyncEvntReqError {source:e, line:config.interrupt_offset.clone()}).unwrap();

            let mut chip4 = Chip::new(config.peckboard_chip.clone())
                .map_err(|e:GpioError| ComponentError::ChipError {source:e, chip: config.peckboard_chip.clone()}).unwrap();
            chip4.get_lines(&config.ir_offsets)
                .map_err(|e:GpioError|ComponentError::LinesGetError {source:e, lines: config.ir_offsets.clone()}).unwrap()
                .request(LineRequestFlags::OUTPUT, &[1,1,1], "peckboard_ir")
                .map_err(|e:GpioError| ComponentError::LinesReqError {source:e, lines: config.ir_offsets.clone()}).unwrap();
            let key_handles: MultiLineHandle = chip4.get_lines(&config.key_offsets)
                .map_err(|e:GpioError| ComponentError::LinesGetError {source:e, lines: config.key_offsets.clone()}).unwrap()
                .request(LineRequestFlags::INPUT, &[0,0,0], "peck_keys")
                .map_err(|e: GpioError| ComponentError::LinesReqError {source:e, lines: config.key_offsets.clone()}).unwrap();
            tracing::trace!("PeckKey Handles created");

            loop {
                match interrupt.next().await {
                    Some(_event) => {
                        //match event.unwrap().event_type() {
                        //    EventType::FallingEdge => {}
                        //    EventType::RisingEdge => {}
                        //} no need to match if state is updated with any event
                        tracing::trace!("PeckKey Interrupted - Event Registered");
                        let values = key_handles.get_values().unwrap();
                        let state = Self::State {
                            peck_left: values[0] != 0, //
                            peck_center: values[1] != 0,
                            peck_right: values[2] != 0,
                        };
                        let message = Any {
                            value: state.encode_to_vec(),
                            type_url: Self::STATE_TYPE_URL.into(),
                        };
                        sender.send(message).await.unwrap();
                    }
                    None => {tracing::error!("PeckKey Interrupted - No Event Registered");continue},
                }
            }
        });
    }

    fn change_state(&mut self, state: Self::State) -> decide_proto::Result<()> {
        self.peck_left.store(state.peck_left, Ordering::Release);
        self.peck_center.store(state.peck_right, Ordering::Release);
        self.peck_right.store(state.peck_center, Ordering::Release);

        let sender = self.state_sender.as_mut().cloned().unwrap();
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

    fn set_parameters(&mut self, _params: Self::Params) -> decide_proto::Result<()> {
        todo!()
    }

    fn get_state(&self) -> Self::State {
        tracing::trace!("Getting state");
        Self::State{
            peck_left: self.peck_left.load(Ordering::Acquire),
            peck_right: self.peck_right.load(Ordering::Acquire),
            peck_center: self.peck_center.load(Ordering::Acquire),
        }
    }

    fn get_parameters(&self) -> Self::Params {
        todo!()
    }
}

#[derive(Deserialize)]
pub struct Config {
    interrupt_chip: String,
    interrupt_offset: u32,
    peckboard_chip: String,
    led_offsets: Vec<u32>,
    ir_offsets: Vec<u32>,
    key_offsets: Vec<u32>,
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
            LedColor::Red => {[1,0,0]}
            LedColor::Blue => {[0,1,0]}
            LedColor::Green => {[0,0,1]}
            LedColor::White => {[1,1,1]}
        }
    }
}