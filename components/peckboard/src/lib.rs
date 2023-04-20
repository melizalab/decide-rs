use gpio_cdev::{Chip, AsyncLineEventHandle,
                LineRequestFlags,
                MultiLineHandle,
                EventRequestFlags,
                //errors::Error as GpioError
};
use futures::stream::StreamExt;
use tokio::sync::mpsc::Sender;
use async_trait::async_trait;
use decide_protocol::{Component,
                   error::DecideError};
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
    self, sync::mpsc, task::JoinHandle
};


pub struct PeckLeds {
    handles: MultiLineHandle,
    led_state: LedColor,
    state_sender: mpsc::Sender<Any>,
}

pub struct PeckKeys {
    peck_left: Arc<AtomicBool>,
    peck_center: Arc<AtomicBool>,
    peck_right: Arc<AtomicBool>,
    state_sender: mpsc::Sender<Any>,
    task_handle: Option<JoinHandle<()>>,
}

#[async_trait]
impl Component for PeckLeds {
    type State = proto::LedState;
    type Params = proto::LedParams;
    type Config = LedConfig;
    const STATE_TYPE_URL: &'static str = "led_state";
    const PARAMS_TYPE_URL: &'static str =  "led_params";

    fn new(config: Self::Config, sender: Sender<Any>) -> Self {
        let mut chip4 = Chip::new(config.peckboard_chip.clone())
            .map_err(|e| DecideError::Component { source: e.into() }).unwrap();
        let handles = chip4.get_lines(&config.led_offsets.clone())
            .map_err(|e| DecideError::Component { source: e.into() }).unwrap()
            .request(LineRequestFlags::OUTPUT, &[0,0,0], "PeckLeds")
            .map_err(|e| DecideError::Component { source: e.into() }).unwrap();
        PeckLeds {
            handles,
            led_state: LedColor::Off,
            state_sender: sender,
        }
    }

    async fn init(&mut self, _config: Self::Config) {
        tracing::debug!("PeckLed init is empty")
    }

    fn change_state(&mut self, state: Self::State) -> decide_protocol::Result<()> {
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
            .map_err(|e| DecideError::Component { source: e.into() }).unwrap();
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
            tracing::trace!("PeckLeds state changed");
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

    async fn shutdown(&mut self) {
        self.handles.set_values(&LedColor::Off.as_value())
            .map_err(|e| DecideError::Component { source: e.into() }).unwrap();
    }
}

#[async_trait]
impl Component for PeckKeys {
    type State = proto::KeyState;
    type Params = proto::KeyParams;
    type Config = KeyConfig;
    const STATE_TYPE_URL: &'static str = "melizalab.org/proto/key_state";
    const PARAMS_TYPE_URL: &'static str = "melizalab.org/proto/key_params";

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
                .map_err(|e| DecideError::Component { source: e.into() }).unwrap();
            let interrupt_offset = chip2.get_line(config.interrupt_offset.clone())
                .map_err(|e| DecideError::Component { source: e.into() }).unwrap();
            let mut interrupt = AsyncLineEventHandle::new(interrupt_offset.events(
                LineRequestFlags::INPUT,
                EventRequestFlags::BOTH_EDGES,
                "Peckboard Interrupt"
            ).unwrap())
                .map_err(|e| DecideError::Component { source: e.into() }).unwrap();

            let mut chip4 = Chip::new(config.peckboard_chip.clone())
                .map_err(|e| DecideError::Component { source: e.into() }).unwrap();
            chip4.get_lines(&config.ir_offsets)
                .map_err(|e| DecideError::Component { source: e.into() }).unwrap()
                .request(LineRequestFlags::OUTPUT, &[1,1,1], "peckboard_ir")
                .map_err(|e| DecideError::Component { source: e.into() }).unwrap();
            let key_handles: MultiLineHandle = chip4.get_lines(&config.key_offsets)
                .map_err(|e| DecideError::Component { source: e.into() }).unwrap()
                .request(LineRequestFlags::INPUT, &[0,0,0], "peck_keys")
                .map_err(|e| DecideError::Component { source: e.into() }).unwrap();
            tracing::debug!("PeckKey Handles created");

            loop {
                match interrupt.next().await {
                    Some(_event) => {
                        //match event.unwrap().event_type() {
                        //    EventType::FallingEdge => {}
                        //    EventType::RisingEdge => {}
                        //} no need to match if state is updated with any event
                        tracing::debug!("PeckKey Interrupted - Event Registered");
                        let values = key_handles.get_values()
                            .map_err(|e| DecideError::Component { source: e.into() }).unwrap();
                        let state = Self::State {
                            peck_left: values[0] != 0, //
                            peck_center: values[1] != 0,
                            peck_right: values[2] != 0,
                        };
                        let message = Any {
                            value: state.encode_to_vec(),
                            type_url: Self::STATE_TYPE_URL.into(),
                        };
                        sender.send(message).await
                            .map_err(|e| DecideError::Component { source: e.into() }).unwrap();
                    }
                    None => {tracing::error!("PeckKey Interrupted - No Event Registered");continue},
                }
            }
        }));
    }

    fn change_state(&mut self, state: Self::State) -> decide_protocol::Result<()> {
        self.peck_left.store(state.peck_left, Ordering::Release);
        self.peck_center.store(state.peck_right, Ordering::Release);
        self.peck_right.store(state.peck_center, Ordering::Release);

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
            tracing::trace!("PeckKeys state changed");
        });
        Ok(())
    }

    fn set_parameters(&mut self, _params: Self::Params) -> decide_protocol::Result<()> {
        tracing::error!("PeckKeys set_params is empty. Make sure your script isn't using it without good reason");
        Ok(())
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
        Self::Params {}
    }

    async fn shutdown(&mut self) {
        if let Some(task_handle) = self.task_handle.take() {
            task_handle.abort();
            task_handle.await
                .map_err(|e| DecideError::Component { source: e.into() }).unwrap_err();
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
            LedColor::Red => {[1,0,0]}
            LedColor::Blue => {[0,1,0]}
            LedColor::Green => {[0,0,1]}
            LedColor::White => {[1,1,1]}
        }
    }
}