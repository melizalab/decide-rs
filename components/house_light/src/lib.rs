use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, atomic::{AtomicBool, AtomicU8, Ordering}};
use std::time::{SystemTime, UNIX_EPOCH};
use chrono::prelude::*;
use async_trait::async_trait;
use prost::Message;
use prost_types::Any;
use serde::Deserialize;
use thiserror::Error;
use tokio::{self,
            sync::mpsc::Sender,
            time::{Duration, sleep}
};
use tokio::task::JoinHandle;
use decide_protocol::{Component, error::DecideError};

pub mod proto {
    include!(concat!(env!("OUT_DIR"), "/_.rs"));
}

pub struct HouseLight {
    manual: Arc<AtomicBool>, // true if manual input of brightness
    dyson: Arc<AtomicBool>, // true if lights governed by sun position at lat/lon
    brightness: Arc<AtomicU8>,
    daytime: Arc<AtomicBool>,
    interval_sec: u64,
    config: Config,
    state_sender: Sender<Any>,
    task_handle: Option<JoinHandle<()>>,
}

#[async_trait]
impl Component for HouseLight {
    type State = proto::HlState;
    type Params = proto::HlParams;
    type Config = Config;
    const STATE_TYPE_URL: &'static str = "type.googleapis.com/HlState";
    const PARAMS_TYPE_URL: &'static str = "type.googleapis.com/HlParams";

    fn new(config: Self::Config, state_sender: Sender<Any>) -> Self {
        use std::path::Path;

        let pwm_address: String = String::from("/sys/class/pwm/pwmchip2/pwm1/");
        if !Path::new(&pwm_address).exists() {
            let export_loc = String::from("/sys/class/pwm/pwmchip2/export");
            fs::write(export_loc.clone(), "1").map_err(|_e| DecideError::Component { source:
                HouseLightError::WriteError { path: export_loc.clone(), value: "1".to_string() }.into()
            }).unwrap()
        }
        let period = config.period.to_string();
        let configs: Vec<&str> = vec!["period", &period, "polarity", "inversed", "enable", "1"];
        for pair in configs.chunks(2) {
            let write_loc = format!("{}{}", pwm_address, pair[0]);
            fs::write(write_loc.clone(), pair[1])
                .map_err(|_e| DecideError::Component { source:
                HouseLightError::WriteError { path: write_loc, value: pair[1].to_string() }.into()
            }).unwrap()
        };

        HouseLight {
            manual: Arc::new(AtomicBool::new(false)),
            dyson: Arc::new(AtomicBool::new(true)),
            brightness: Arc::new(AtomicU8::new(0)),
            daytime: Arc::new(AtomicBool::new(false)),
            interval_sec: 300,
            config,
            state_sender,
            task_handle: None,
        }
    }

    async fn init(&mut self, config: Self::Config) {

        let manual = self.manual.clone();
        let fake_sun = self.dyson.clone();
        let brightness = self.brightness.clone();
        let daytime = self.daytime.clone();
        let interval_sec = self.interval_sec.clone();
        let sender = self.state_sender.clone();
        //let period = config.period;

        self.task_handle = Some(tokio::spawn(async move {
            loop {
                if manual.load(Ordering::Acquire) { // manual brightness setting
                    let bt = brightness.load(Ordering::Acquire);
                    let dt = bt > 0;
                    Self::send_state(
                        &Self::State{ manual: true, dyson: false,
                                           brightness: bt as i32, daytime: dt },
                        &sender
                    ).await;
                } else { // normal daylight setting

                    let dyson  = fake_sun.load(Ordering::Relaxed);
                    let altitude = HouseLight::calc_altitude(dyson,
                                                             config.fake_dawn,
                                                             config.fake_dusk,
                                                             config.lat,
                                                             config.lon);
                    let new_brightness = HouseLight::calc_brightness(altitude, config.max_brightness);
                    let dt = new_brightness > 0;
                    daytime.store(dt, Ordering::Release);
                    Self::write_brightness(new_brightness, config.device_path.clone(), config.period);
                    brightness.store(new_brightness, Ordering::Relaxed);
                    Self::send_state(
                        &Self::State{ manual: false, dyson,
                            brightness: new_brightness as i32, daytime: dt },
                        &sender
                    ).await;
                }
                sleep(Duration::from_secs(interval_sec)).await;
            }
        }));
        tracing::info!("house light initiated");
    }

    fn change_state(&mut self, state: Self::State) -> decide_protocol::Result<()> {
        let sender = self.state_sender.clone();
        self.manual.store(state.manual, Ordering::Relaxed);
        self.dyson.store(state.dyson, Ordering::Relaxed);

        // Change brightness immediately
        let new_brightness = if state.manual {
            let new_brightness = state.brightness as u8;
            Self::write_brightness(new_brightness, self.config.device_path.clone(), self.config.period.clone());
            self.brightness.store(new_brightness, Ordering::Relaxed);
	    new_brightness

        } else {
            let d  = self.dyson.load(Ordering::Relaxed);
            let altitude = HouseLight::calc_altitude(d,
                                                     self.config.fake_dawn,
                                                     self.config.fake_dusk,
                                                     self.config.lat,
                                                     self.config.lon);
            let new_brightness = HouseLight::calc_brightness(altitude, self.config.max_brightness);
            let dt = new_brightness > 0;
            self.daytime.store(dt, Ordering::Release);
            Self::write_brightness(new_brightness, self.config.device_path.clone(), self.config.period.clone());
            self.brightness.store(new_brightness, Ordering::Relaxed);
	    new_brightness
        };

        let new_state = Self::State {
            manual: state.manual,
            dyson: state.dyson,
            brightness: new_brightness as i32,
            daytime: self.daytime.load(Ordering::Relaxed)
        };
        tokio::spawn(async move {
            Self::send_state(&new_state, &sender).await;
        });

        Ok(())
    }

    fn set_parameters(&mut self, params: Self::Params) -> decide_protocol::Result<()> {
        self.interval_sec = params.clock_interval_sec as u64;
        Ok(())
    }

    fn get_state(&self) -> Self::State {
        Self::State {
            manual: self.manual.load(Ordering::Relaxed),
            dyson: self.dyson.load(Ordering::Relaxed),
            brightness: self.brightness.load(Ordering::Relaxed) as i32,
            daytime: self.daytime.load(Ordering::Relaxed),
        }
    }

    fn get_parameters(&self) -> Self::Params {
        Self::Params {
            clock_interval_sec: self.interval_sec as i64
        }
    }

    async fn send_state(state: &Self::State, sender: &Sender<Any>) {
        sender.send(Any {
            type_url: String::from(Self::STATE_TYPE_URL),
            value: state.encode_to_vec(),
        }).await.map_err(|_e| DecideError::Component {
            source: HouseLightError::SendError.into()
        }).unwrap();
    }

    async fn shutdown(&mut self) {
        if let Some(task_handle) = self.task_handle.take() {
            task_handle.abort();
            assert!(task_handle.await.unwrap_err().is_cancelled());
        }
    }
}

impl HouseLight {

    fn calc_brightness(altitude: f64, max_brightness: u8) -> u8 {
        let x = (altitude.sin() * (max_brightness as f64)).round() as u8;
        //let brightness = max(0.0, x); //trait 'Ord' is not implemented for '{float}'
        if x > 0 { x } else { 0 }
    }
    fn calc_altitude(dyson: bool, dawn: f64, dusk: f64, lat: f64, lon: f64) -> f64 {
        if dyson {
            let now = Local::now();
            tracing::debug!("fake clock specified, time is {:?}", now);
            let now = (now.hour() + (now.minute() / 60) + (now.second() / 3600)) as f64;
            let x: f64 = (now + 24.0 - dawn) % 24.0;
            let y: f64 = (dusk + 24.0 - dawn) % 24.0;
            (x / y) * std::f64::consts::PI
        } else {
            let now = SystemTime::now().duration_since(UNIX_EPOCH)
                .map_err(|_e| DecideError::Component {
                    source: HouseLightError::SysTimeError.into()
                }).unwrap();
            tracing::debug!("fake clock not specified, time is {:?}", now);
            sun::pos(now.as_millis() as i64, lat, lon).altitude
        }
    }
    fn write_brightness(new_brightness: u8, pwm_path: String, period: f64) {
        let duty_cycle: u32 = (period * (1.0 - (new_brightness as f64 / 255.0))) as u32;
        let path = fs::canonicalize(PathBuf::from(pwm_path.clone()))
            .map_err(|_e| DecideError::Component {
                source: HouseLightError::InvalidFs {
                    requested: pwm_path.clone()}.into()
            }).unwrap();
        fs::write(path.clone(), duty_cycle.to_string())
            .map_err(|_e| DecideError::Component {
                source: HouseLightError::WriteError {
                    path: pwm_path.clone(), value: duty_cycle.to_string() }.into()
            }).unwrap();
        tracing::info!("house light brightness set to {:?} ", new_brightness);
    }
}

#[derive(Deserialize)]
pub struct Config {
    device_path: String, // /sys/class/pwm/pwmchip2/pwm1/duty_cycle
    period: f64,
    fake_dawn: f64,
    fake_dusk: f64,
    lat: f64,
    lon: f64,
    max_brightness: u8 //255
}

#[derive(Error, Debug)]
pub enum HouseLightError {
    #[error("could not find file for writing brightness value: {requested:?}")]
    InvalidFs{requested: String},
    #[error("could not write value {value:?} to file {path:?}")]
    WriteError{path: String, value: String},
    #[error("could not send state change")]
    SendError,
    #[error("could not acquire Unix-Epoch time from system")]
    SysTimeError
}