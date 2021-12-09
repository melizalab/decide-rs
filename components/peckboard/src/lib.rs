use gpio_cdev::{Chip, AsyncLineEventHandle,
                LineRequestFlags,
                MultiLineHandle,
                EventRequestFlags, EventType,
                //errors::Error as GpioError
};
use futures::stream::StreamExt;
use tokio::sync::mpsc::Sender;
use async_trait::async_trait;
use decide_proto::{Component,
                   //error::DecideError
};
use prost::Message;
use prost_types::Any;
use serde::Deserialize;
use std::sync::{
    atomic::{AtomicBool,// Ordering
    },
    Arc,
};
use tokio::{
    self,
    //time::{sleep, Duration},
};

pub mod peckboard {
    include!(concat!(env!("OUT_DIR"), "/_.rs"));
}

struct PeckLeds {
    handles: MultiLineHandle,
    led_state: LedState,
}
struct PeckKeys {
    peck_left: Arc<AtomicBool>,
    peck_center: Arc<AtomicBool>,
    peck_right: Arc<AtomicBool>,
}

#[async_trait]
impl Component for PeckLeds {
    type State = peckboard::Led_State;
    type Params = peckboard::Led_Params;
    type Config = Config;
    const STATE_TYPE_URL: &'static str = "melizalab.org/proto/led_state";
    const PARAMS_TYPE_URL: &'static str =  "melizalab.org/proto/led_state";

    fn new(config: Self::Config) -> Self {
        let mut chip4 = Chip::new(config.peckboard_chip).unwrap();
        let handles = chip4.get_lines(&config.led_offsets).unwrap()
            .request(LineRequestFlags::OUTPUT, &[], "PeckLeds").unwrap();
        PeckLeds {
            handles,
            led_state: LedState::Off
        }
    }

    async fn init(&self, config: Self::Config, state_sender: Sender<Any>) {
        todo!("empty")
    }

    fn change_state(&mut self, state: Self::State) -> decide_proto::Result<()> {
        match state.led_state {
            "off" => {self.led_state = LedState::Off}
            "red" => {self.led_state = LedState::Red}
            "blue" => {self.led_state = LedState::Blue}
            "green" => {self.led_state = LedState::Green}
            "white" => {self.led_state = LedState::White}
            _ => {} //throw warning
        }
        let lines_value = self.led_state.as_value();
        self.handles.set_values(&lines_value).unwrap();
        Ok(())
    }

    fn set_parameters(&mut self, params: Self::Params) -> decide_proto::Result<()> {
        todo!()
    }

    fn get_state(&self) -> Self::State {
        Self::State {
            led_state: match self.led_state {
                LedState::Off => {"off"}
                LedState::Blue => {"blue"}
                LedState::Red => {"red"}
                LedState::Green => {"green"}
                LedState::White => {"white"}
            }
        }
    }

    fn get_parameters(&self) -> Self::Params {
        Ok(())
    }
}

#[async_trait]
impl Component for PeckKeys {
    type State = peckboard::Key_State;
    type Params = peckboard::Key_Params;
    type Config = Config;
    const STATE_TYPE_URL: &'static str = ""; //TODO: Add peckboard links
    const PARAMS_TYPE_URL: &'static str = "";

    fn new(config: Self::Config) -> Self {
        PeckKeys {
            peck_left: Arc::new(AtomicBool::new(false)),
            peck_center:  Arc::new(AtomicBool::new(false)),
            peck_right:  Arc::new(AtomicBool::new(false)),
        }
    }

    async fn init(&self, config: Self::Config, state_sender: Sender<Any>) {
        //let peck_left = self.peck_left.clone();
        //let peck_center = self.peck_center.clone();
        //let peck_right = self.peck_right.clone();

        tokio::spawn(async move {
            let mut chip2 = Chip::new(config.interrupt_chip).unwrap();
            let interrupt_offset = chip2.get_line(config.interrupt_offset).unwrap();
            let mut interrupt = AsyncLineEventHandle::new(interrupt_offset.events(
                LineRequestFlags::INPUT,
                EventRequestFlags::BOTH_EDGES,
                "Peckboard Interrupt"
            ).unwrap()).unwrap();

            let mut chip4 = Chip::new(config.peckboard_chip).unwrap();
            chip4.get_lines(&config.ir_offsets).unwrap()
                .request(LineRequestFlags::OUTPUT, &[1,1,1], "peckboard_ir").unwrap();
            let key_handles: MultiLineHandle = chip4.get_lines(&config.key_offsets).unwrap()
                .request(LineRequestFlags::INPUT, &[0,0,0], "peck_keys").unwrap();

            loop {
                match interrupt.next().await {
                    Some(event) => {
                        match event.unwrap().event_type() {
                            EventType::FallingEdge => {}
                            EventType::RisingEdge => {}
                        } //no need to match if state is update with ANY event
                        let values = key_handles.get_values().unwrap();
                        let state = Self::State {
                            peck_left: values[0] != 0, //wonky
                            peck_center: values[1] != 0,
                            peck_right: values[2] != 0,
                        };
                        let message = Any {
                            value: state.encode_to_vec(),
                            type_url: Self::STATE_TYPE_URL.into(),
                        };
                        state_sender.send(message).await.unwrap();
                    }
                    None => continue,
                }
            }
        });
    }

    fn change_state(&mut self, state: Self::State) -> decide_proto::Result<()> {
        todo!()
    }

    fn set_parameters(&mut self, params: Self::Params) -> decide_proto::Result<()> {
        todo!()
    }

    fn get_state(&self) -> Self::State {
        todo!()
    }

    fn get_parameters(&self) -> Self::Params {
        todo!()
    }
}

#[derive(Deserialize)]
struct Config {
    interrupt_chip: String,
    interrupt_offset: u32,
    peckboard_chip: String,
    led_offsets: Vec<u32>,
    ir_offsets: Vec<u32>,
    key_offsets: Vec<u32>,
}

#[derive(Clone, Copy, Debug)]
pub enum LedState {
    Off,
    Blue,
    Red,
    Green,
    White,
}
impl LedState {
    fn next(&mut self) -> &mut Self { //TODO: determine whether or not this method is necessary
        match self {
            LedState::Off   => {*self = LedState::Blue}
            LedState::Blue   => {*self = LedState::Red}
            LedState::Red  => {*self = LedState::Green}
            LedState::Green => {*self = LedState::White}
            LedState::White   => {*self = LedState::Off}
        };
        self
    }
    fn as_value(&self) -> [u8; 3] {
        match self {
            LedState::Off => {[0,0,0]}
            LedState::Red => {[1,0,0]}
            LedState::Blue => {[0,1,0]}
            LedState::Green => {[0,0,1]}
            LedState::White => {[1,1,1]}
        }
    }
}