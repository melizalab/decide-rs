use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, atomic::{AtomicBool, AtomicU8, Ordering}};
use std::time::{SystemTime, UNIX_EPOCH};
use chrono::prelude::*;
use async_trait::async_trait;
use prost::Message;
use prost_types::Any;
use serde::Deserialize;
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
    interval: u64,
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
        HouseLight {
            manual: Arc::new(AtomicBool::new(false)),
            dyson: Arc::new(AtomicBool::new(true)),
            brightness: Arc::new(AtomicU8::new(0)),
            daytime: Arc::new(AtomicBool::new(false)),
            interval: 300,
            config: config,
            state_sender,
            task_handle: None,
        }
    }

    async fn init(&mut self, config: Self::Config) {

        let manual = self.manual.clone();
        let fake_sun = self.dyson.clone();
        let brightness = self.brightness.clone();
        let daytime = self.daytime.clone();
        let interval = self.interval;
        let sender = self.state_sender.clone();
        //let period = config.period;

        self.task_handle = Some(tokio::spawn(async move {
            let dev_path = config.device_path.clone();
            loop {
                if manual.load(Ordering::Acquire) {
                    let bt = brightness.load(Ordering::Acquire);
                    let dt = bt > 0;
                    tracing::debug!("Currently in manual mode, no need to adjust brightness");
                    let state = Self::State {
                        manual: true,
                        dyson: false,
                        brightness: bt as i32,
                        daytime: dt
                    };
                    let message = Any {
                        value: state.encode_to_vec(),
                        type_url: Self::STATE_TYPE_URL.into(),
                    };
                    sender.send(message).await
                        .map_err(|e| DecideError::Component { source: e.into() })
                        .unwrap();
                } else {

                    let dyson  = fake_sun.load(Ordering::Relaxed);
                    let altitude = HouseLight::calc_altitude(dyson,
                                                             config.fake_dawn,
                                                             config.fake_dusk,
                                                             config.lat,
                                                             config.lon);
                    let new_brightness = HouseLight::calc_brightness(altitude, config.max_brightness);
                    let dt = new_brightness > 0;
                    daytime.store(dt, Ordering::Release);

                    //let duty = HouseLight::get_duty(period, new_brightness);
                    let path = fs::canonicalize(PathBuf::from(dev_path.clone())).unwrap();
                    fs::write(path, new_brightness.to_string()).expect("Unable to write brightness");
                    tracing::info!("House-Light Brightness Set to {:?} ", new_brightness);
                    brightness.store(new_brightness, Ordering::Relaxed);

                    let state = Self::State {
                        manual: false,
                        dyson,
                        brightness: new_brightness as i32,
                        daytime: dt
                    };
                    let message = Any {
                        value: state.encode_to_vec(),
                        type_url: Self::STATE_TYPE_URL.into(),
                    };
                    sender.send(message).await
                        .map_err(|e| DecideError::Component { source: e.into() })
                        .unwrap();
                }
                sleep(Duration::from_secs(interval)).await;
            }
        }));
        tracing::info!("House-Light Initiated");
    }

    fn change_state(&mut self, state: Self::State) -> decide_protocol::Result<()> {
        let sender = self.state_sender.clone();
        let dev_path = self.config.device_path.clone();
        self.manual.store(state.manual, Ordering::Relaxed);
        self.dyson.store(state.dyson, Ordering::Relaxed);

        let mut new_brightness: u8 = 0;
        // Change brightness immediately
        if state.manual {
            new_brightness = state.brightness as u8;
            //let duty = HouseLight::get_duty(self.period, new_brightness);
            fs::write(dev_path, new_brightness.to_string())
                .expect("Unable to write brightness");
            tracing::info!("House-Light Brightness Set to {:?} Manually", new_brightness);
            self.brightness.store(new_brightness, Ordering::Relaxed);

        } else {
            let d  = self.dyson.load(Ordering::Relaxed);
            let altitude = HouseLight::calc_altitude(d,
                                                     self.config.fake_dawn,
                                                     self.config.fake_dusk,
                                                     self.config.lat,
                                                     self.config.lon);
            new_brightness = HouseLight::calc_brightness(altitude, self.config.max_brightness);
            let dt = new_brightness > 0;
            self.daytime.store(dt, Ordering::Release);
            //let duty = HouseLight::get_duty(period, new_brightness);
            let path = fs::canonicalize(PathBuf::from(dev_path.clone())).unwrap();
            fs::write(path, new_brightness.to_string()).expect("Unable to write brightness");
            tracing::info!("House-Light Brightness Set to {:?} ", new_brightness);
            self.brightness.store(new_brightness, Ordering::Relaxed);
        }

        let new_state = Self::State {
            manual: state.manual,
            dyson: state.dyson,
            brightness: new_brightness as i32,
            daytime: self.daytime.load(Ordering::Relaxed)
        };
        tokio::spawn(async move {
            sender
                .send(Any {
                    type_url: String::from(Self::STATE_TYPE_URL),
                    value: new_state.encode_to_vec(),
                })
                .await
                .map_err(|e| DecideError::Component { source: e.into() })
                .unwrap();
            tracing::debug!("House-Light State Changed by Request");
        });

        Ok(())
    }

    fn set_parameters(&mut self, params: Self::Params) -> decide_protocol::Result<()> {
        self.interval = params.clock_interval as u64;
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
            clock_interval: self.interval as i64
        }
    }

    async fn shutdown(&mut self) {
        if let Some(task_handle) = self.task_handle.take() {
            task_handle.abort();
            task_handle.await
                .map_err(|e| DecideError::Component { source: e.into() })
                .unwrap_err();
        }
    }
}

impl HouseLight {
    //fn get_duty(period: u32, brightness: u8) -> String {
    //    (period * (brightness as u32)/100).to_string()
    //}

    fn calc_brightness(altitude: f64, max_brightness: u8) -> u8 {
        let x = (altitude.sin() * (max_brightness as f64)).round() as u8;
        //let brightness = max(0.0, x); //trait 'Ord' is not implemented for '{float}'
        if x > 0 { x } else { 0 }
    }
    fn calc_altitude(dyson: bool, dawn: f64, dusk: f64, lat: f64, lon: f64) -> f64 {
        if dyson {
            let now = chrono::offset::Local::now();
            tracing::debug!("Fake Clock specified, time is {:?}", now);
            let now = (now.hour() + (now.minute() / 60) + (now.second() / 3600)) as f64;
            let x: f64 = (now + 24.0 - dawn) % 24.0;
            let y: f64 = (dusk + 24.0 - dawn) % 24.0;
            (x / y) * std::f64::consts::PI
        } else {
            let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
            tracing::debug!("Fake Clock not specified, time is {:?}", now);
            sun::pos(now.as_millis() as i64, lat, lon).altitude
        }
    }
}

#[derive(Deserialize)]
pub struct Config {
    device_path: String, // /sys/class/leds/starboard::lights/brightness
    fake_dawn: f64,
    fake_dusk: f64,
    lat: f64,
    lon: f64,
    //period: u32,
    max_brightness: u8 //255
}