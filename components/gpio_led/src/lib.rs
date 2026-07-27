use async_trait::async_trait;
use decide_protocol::{error::DecideError,
                      Component};
use gpio_cdev::{Chip, LineRequestFlags};
use prost::Message;
use prost_types::Any;
use serde::Deserialize;
use thiserror::Error;

use std::sync::{Arc, Mutex, atomic::{AtomicBool, AtomicU64, Ordering}};
use tokio::{self, sync::mpsc::{self, Sender},
            time::{sleep, Duration, Instant}};

pub struct MonoLed {
    switch: Arc<AtomicBool>,
    led_state: Arc<Mutex<LedColor>>,
    blink: Arc<AtomicBool>,
    blink_duration: Arc<AtomicU64>,
    state_sender: Sender<Any>,
    shutdown: Option<Sender<bool>>
}
pub struct RGBLed {
    switch: Arc<AtomicBool>,
    led_state: Arc<Mutex<LedColor>>,
    blink: Arc<AtomicBool>,
    blink_duration: Arc<AtomicU64>,
    state_sender: Sender<Any>,
    shutdown: Option<Sender<bool>>}

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

    fn new(_config: Self::Config, sender: Sender<Any>) -> Self {
        MonoLed {
            switch: Arc::new(AtomicBool::new(false)),
            led_state: Arc::new(Mutex::new(LedColor::Off)),
            blink: Arc::new(AtomicBool::new(false)),
            blink_duration: Arc::new(AtomicU64::new(4000)),
            state_sender: sender,
            shutdown: None
        }
    }

    async fn init(&mut self, config: Self::Config) {
        let switch = self.switch.clone();
        let led_state = self.led_state.clone();
        let blink = self.blink.clone();
        let blink_duration = self.blink_duration.clone();
        let sender = self.state_sender.clone();
        let (shutdown_tx, mut shutdown_rx) = mpsc::channel(1);

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

        let _led_handle = tokio::spawn(async move {
            loop {
                if shutdown_rx.try_recv().unwrap_err() == mpsc::error::TryRecvError::Disconnected {
                    handle.set_value(0)
                        .map_err(|_e| DecideError::Component {
                            source: LedError::GpioLineSetError {
                                line: handle.line().offset(),
                                value: 0
                            }.into()
                        }).unwrap();
                    break
                }
                let switch_on = switch.load(Ordering::Acquire);
                if switch_on {
                    let new_color = *led_state.lock().unwrap();
                    let blinky = blink.load(Ordering::Acquire);
                    match blinky {
                        false => {
                            handle.set_value(new_color.mono_as_value())
                                .map_err(|_e| DecideError::Component { source:
                                    LedError::GpioLineSetError {
                                        line: handle.line().offset(),
                                        value: new_color.mono_as_value()
                                    }.into()
                                }).unwrap();
                            let new_state = Self::State{
                                state: new_color.to_str(),
                                blink: false
                            };
                            Self::send_state(&new_state, &sender).await;
                            switch.store(false, Ordering::Release);
                        }
                        true => {
                            let blink_dur = blink_duration.load(Ordering::Acquire);
                            let timer = Instant::now();
                            let mut alt_color = new_color;
                            Self::send_state(&Self::State{
                                state: new_color.to_str(),
                                blink: true,
                            }, &sender).await;

                            while Instant::now().duration_since(timer) < Duration::from_millis(blink_dur) {
                                handle.set_value(alt_color.mono_as_value())
                                    .map_err(|_e: gpio_cdev::Error| DecideError::Component { source:
                                        LedError::GpioLineSetError {
                                            line: handle.line().offset(),
                                            value: alt_color.mono_as_value()
                                        }.into()
                                }).unwrap();
                                alt_color = if alt_color==new_color {LedColor::Off} else {new_color}; 
                            }

                            handle.set_value(LedColor::Off.mono_as_value())
                                .map_err(|_e: gpio_cdev::Error| DecideError::Component { source:
                                    LedError::GpioLineSetError {
                                        line: handle.line().offset(),
                                        value: LedColor::Off.mono_as_value()
                                    }.into()
                            }).unwrap();

                            blink.store(false, Ordering::Release);
                            Self::send_state(&Self::State{
                                state: LedColor::Off.to_str(),
                                blink: false,
                            }, &sender).await;
                            switch.store(false, Ordering::Release);
                        }
                    }
                } else {
                    tokio::time::sleep(Duration::from_micros(100)).await;
                }
            }
        });
        self.shutdown = Some(shutdown_tx);
        tracing::info!("mono led initiated")
    }

    fn change_state(&mut self, state: Self::State) -> decide_protocol::Result<()> {
        tracing::debug!("LED state change initiated.");
        self.blink.store(state.blink, Ordering::Release);
        let mut led_state = self.led_state.lock().unwrap();
        *led_state = LedColor::from_str(&state.state);
        self.switch.store(true, Ordering::Release);
        Ok(())
    }

    fn set_parameters(&mut self, params: Self::Params) -> decide_protocol::Result<()> {
        tracing::debug!("Changing LED blink duration.");
        self.blink_duration.store(params.blink_duration, Ordering::Release);
        Ok(())
    }

    fn get_state(&self) -> Self::State {
        let state = self.led_state.lock().unwrap();
        let blink = self.blink.load(Ordering::Acquire);
        Self::State {
            state: state.to_str(),
            blink
        }
    }

    fn get_parameters(&self) -> Self::Params {
        Self::Params{
            blink_duration: self.blink_duration.load(Ordering::Acquire)
        }
    }

    async fn send_state(state: &Self::State, sender: &Sender<Any>) {
        sender.send(Any {
            type_url: String::from(Self::STATE_TYPE_URL),
            value: state.encode_to_vec(),
        }).await.map_err(|_e| DecideError::Component {
            source: LedError::SendError.into() 
        }).unwrap();
    }

    async fn shutdown(&mut self) {
        let shutdown_tx = self.shutdown.take().unwrap();
        drop(shutdown_tx);
    }
}

#[async_trait]
impl Component for RGBLed {
    type State = proto::LedState;
    type Params = proto::LedParams;
    type Config = RGBLedConfig;
    const STATE_TYPE_URL: &'static str = "type.googleapis.com/LedState";
    const PARAMS_TYPE_URL: &'static str =  "type.googleapis.com/LedParams";

    fn new(_config: Self::Config, sender: Sender<Any>) -> Self {
        RGBLed {
            switch: Arc::new(AtomicBool::new(false)),
            led_state: Arc::new(Mutex::new(LedColor::Off)),
            blink: Arc::new(AtomicBool::new(false)),
            blink_duration: Arc::new(AtomicU64::new(4000)),
            state_sender: sender,
            shutdown: None
        }
    }

    async fn init(&mut self, config: Self::Config) {
        let switch = self.switch.clone();
        let led_state = self.led_state.clone();
        let blink = self.blink.clone();
        let blink_duration = self.blink_duration.clone();
        let sender = self.state_sender.clone();
        let (shutdown_tx, mut shutdown_rx) = mpsc::channel(1);
        
        let mut dev_chip = Chip::new(&config.gpio_chip)
            .map_err(|_e| DecideError::Component { source:
                LedError::GpioChipError { dev: config.gpio_chip.clone() }.into()
            }).unwrap();

        let handle = dev_chip.get_lines(&config.gpio_lines)
            .map_err(|_e| DecideError::Component { source:
                LedError::GpioLinesReqError {
                    lines: config.gpio_lines.clone(),
                    dev:config.gpio_chip.clone()
                }.into()
            }).unwrap()
            .request(LineRequestFlags::OUTPUT, &LedColor::Off.as_value(), "gpio_rgb_led")
            .map_err(|_e| DecideError::Component { source:
                LedError::GpioFlagsReqError {
                    lines: config.gpio_lines.clone(),
                    dev:config.gpio_chip.clone(),
                    flag:"OUT".to_string()
                }.into()
            }).unwrap();

        let _led_handle = tokio::spawn(async move {
            loop {
                if shutdown_rx.try_recv().unwrap_err() == mpsc::error::TryRecvError::Disconnected {
                    handle.set_values(&LedColor::Off.as_value())
                        .map_err(|_e| DecideError::Component {
                            source: LedError::GpioLinesSetError {
                                value: LedColor::Off.as_value()
                            }.into()
                        }).unwrap();
                    break
                };

                let switch_on = switch.load(Ordering::Acquire);
                if switch_on {
                    let new_color = *led_state.lock().unwrap();
                    let blinky = blink.load(Ordering::Acquire);
                    match blinky {
                        false => {
                            tracing::debug!("changing LED state!");
                            handle.set_values(&new_color.as_value())
                                .map_err(|_e| DecideError::Component { source:
                                    LedError::GpioLinesSetError {
                                        value: new_color.as_value()
                                    }.into()
                                }).unwrap();
                            let new_state = Self::State{
                                state: new_color.to_str(),
                                blink: false
                            };
                            Self::send_state(&new_state, &sender).await;
                            switch.store(false, Ordering::Release);
                        }
                        true => {
                            let blink_dur = blink_duration.load(Ordering::Acquire);
                            Self::send_state(&Self::State{
                                state: new_color.to_str(),
                                blink: true,
                            }, &sender).await;

                            let timer = Instant::now();
                            let mut alt_color = new_color;
                            tracing::debug!("blinking LED!");
                            while Instant::now().duration_since(timer) < Duration::from_millis(blink_dur) {
                                handle.set_values(&alt_color.as_value())
                                    .map_err(|_e| DecideError::Component { source:
                                        LedError::GpioLinesSetError {
                                            value: new_color.as_value()
                                        }.into()
                                    }).unwrap();
                                alt_color = if alt_color==new_color {LedColor::Off} else {new_color};
                                sleep(Duration::from_millis(200)).await;
                            }

                            handle.set_values(&LedColor::Off.as_value())
                                .map_err(|_e| DecideError::Component { source:
                                    LedError::GpioLinesSetError {
                                        value: new_color.as_value()
                                    }.into()
                                }).unwrap();
                            tracing::debug!("finished blinking LED!");

                            blink.store(false, Ordering::Release);
                            Self::send_state(&Self::State{
                                state: LedColor::Off.to_str(),
                                blink: false,
                            }, &sender).await;
                            switch.store(false, Ordering::Release);
                        }
                    }
                } else {
                    tokio::time::sleep(Duration::from_micros(100)).await;
                }
            }
        });
        self.shutdown = Some(shutdown_tx);
        tracing::info!("rgb led initiated")
    }

    fn change_state(&mut self, state: Self::State) -> decide_protocol::Result<()> {
        tracing::debug!("LED state change initiated.");
        self.blink.store(state.blink, Ordering::Release);
        let mut led_state = self.led_state.lock().unwrap();
        *led_state = LedColor::from_str(&state.state);
        self.switch.store(true, Ordering::Release);
        Ok(())
    }

    fn set_parameters(&mut self, params: Self::Params) -> decide_protocol::Result<()> {
        tracing::debug!("Setting blink duration to {:?}", params.blink_duration);
        self.blink_duration.store(params.blink_duration, Ordering::Release);
        Ok(())
    }

    fn get_state(&self) -> Self::State {
        let state = self.led_state.lock().unwrap();
        let blink = self.blink.load(Ordering::Acquire);
        Self::State {
            state: state.to_str(),
            blink
        }
    }

    fn get_parameters(&self) -> Self::Params {
        let dur = self.blink_duration.load(Ordering::Acquire);
        Self::Params{
            blink_duration: dur
        }
    }

    async fn send_state(state: &Self::State, sender: &Sender<Any>) {
        sender.send(Any {
            type_url: String::from(Self::STATE_TYPE_URL),
            value: state.encode_to_vec(),
        }).await.map_err(|_e| DecideError::Component {
            source: LedError::SendError.into() 
        }).unwrap();
    }

    async fn shutdown(&mut self) {
        let shutdown_tx = self.shutdown.take().unwrap();
        drop(shutdown_tx);
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
