use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, atomic::{AtomicBool, AtomicU8, Ordering}};
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use chrono::{self, DateTime, Timelike, Utc};
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
    ephemera: Arc<AtomicBool>, // true if lights governed by sun position at lat/lon
    brightness: Arc<AtomicU8>,
    daytime: Arc<AtomicBool>,
    interval: u64,
    period: u32,
    write_loc: Option<String>,
    state_sender: Sender<Any>,
    task_handle: Option<JoinHandle<()>>,
}

#[async_trait]
impl Component for HouseLight {
    type State = proto::HlState;
    type Params = proto::HlParams;
    type Config = Config;
    const STATE_TYPE_URL: &'static str = "hl_state";
    const PARAMS_TYPE_URL: &'static str = "hl_params";

    fn new(config: Self::Config, state_sender: Sender<Any>) -> Self {
        HouseLight {
            manual: Arc::new(AtomicBool::new(false)),
            ephemera: Arc::new(AtomicBool::new(true)),
            brightness: Arc::new(AtomicU8::new(0)),
            daytime: Arc::new(AtomicBool::new(false)),
            interval: 300,
            period: config.period,
            write_loc: None,
            state_sender,
            task_handle: None,
        }
    }

    async fn init(&mut self, config: Self::Config) {
        self.write_loc = Some(config.device_path.clone());

        let dawn = config.fake_dawn;
        let dusk = config.fake_dusk;
        let lat = config.lat;
        let lon = config.lon;

        let manual = self.manual.clone();
        let ephemera = self.ephemera.clone();
        let brightness = self.brightness.clone();
        let daytime = self.daytime.clone();
        let interval = self.interval;
        let sender = self.state_sender.clone();
        let period = config.period;

        self.task_handle = Some(tokio::spawn(async move{
            let dev_path = config.device_path.clone();
            loop {
                if manual.load(Ordering::Acquire) {
                    let new_brightness = brightness.load(Ordering::Acquire);
                    let dt = new_brightness > 0;
                    daytime.store(dt, Ordering::Release);
                    let duty = HouseLight::get_duty(period, new_brightness);
                    let path = fs::canonicalize(PathBuf::from(dev_path.clone())).unwrap();
                    fs::write(path, duty).expect("Unable to write brightness");
                    tracing::debug!("Brightness {:?} written to file", new_brightness);

                    let state = Self::State {
                        manual: false,
                        ephemera: false,
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
                } else {

                    let e  = ephemera.load(Ordering::Relaxed);
                    let altitude = HouseLight::calc_altitude(e, dawn, dusk, lat, lon);
                    let new_brightness = HouseLight::calc_brightness(altitude, 100);
                    let dt = new_brightness > 0;
                    daytime.store(dt, Ordering::Release);

                    let duty = HouseLight::get_duty(period, new_brightness);
                    let path = fs::canonicalize(PathBuf::from(dev_path.clone())).unwrap();
                    fs::write(path, duty).expect("Unable to write brightness");
                    tracing::debug!("Brightness {:?} written to file", new_brightness);
                    brightness.store(new_brightness, Ordering::Relaxed);

                    let state = Self::State {
                        manual: false,
                        ephemera: e,
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
    }

    fn change_state(&mut self, state: Self::State) -> decide_protocol::Result<()> {
        let sender = self.state_sender.clone();

        self.manual.store(state.manual, Ordering::Relaxed);
        self.ephemera.store(state.ephemera, Ordering::Relaxed);
        self.brightness.store(state.brightness as u8, Ordering::Relaxed);

        // Change brightness immediately if overriding
        if state.manual {
            let path = self.write_loc.clone().unwrap();
            let new_brightness = state.brightness as u8;
            let duty = HouseLight::get_duty(self.period, new_brightness);
            fs::write(path, duty)
                .expect("Unable to write brightness");
            tracing::debug!("Brightness {:?} written to file in manual mode", new_brightness);
        }

        tokio::spawn(async move {
            sender
                .send(Any {
                    type_url: String::from(Self::STATE_TYPE_URL),
                    value: state.encode_to_vec(),
                })
                .await
                .map_err(|e| DecideError::Component { source: e.into() })
                .unwrap();
            tracing::debug!("House-light state changed");
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
            ephemera: self.ephemera.load(Ordering::Relaxed),
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
    fn get_duty(period: u32, brightness: u8) -> String {
        (period * (brightness as u32)/100).to_string()
    }
    fn calc_brightness(altitude: f64, max_brightness: u8) -> u8 {
        let x = (altitude.sin() * (max_brightness as f64)).round() as u8;
        //let brightness = max(0.0, x); //trait 'Ord' is not implemented for '{float}'
        if x > 0 { x } else { 0 }
    }
    fn calc_altitude(ephemera: bool, dawn: f64, dusk: f64, lat: f64, lon: f64) -> f64 {
        if !ephemera {
            let now: DateTime<Utc> = DateTime::from(SystemTime::now());
            let now = (now.hour() + (now.minute() / 60) + (now.second() / 3600)) as f64;
            let x: f64 = (now + 24.0 - dawn) % 24.0;
            let y: f64 = (dusk + 24.0 - now) % 24.0;
            (x / y) * std::f64::consts::PI
        } else {
            let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
            sun::pos(now.as_millis() as i64, lat, lon).altitude
        }
    }
}

#[derive(Deserialize)]
pub struct Config {
    device_path: String, // /sys/class/pwm/pwmchip7/pwm1/duty_cycle
    fake_dawn: f64,
    fake_dusk: f64,
    lat: f64,
    lon: f64,
    period: u32,
}