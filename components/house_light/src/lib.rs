use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;
use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
use std::sync::atomic::{AtomicU32, AtomicU8};
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use chrono::{self, DateTime, Timelike, Utc};
use prost::Message;
use prost_types::Any;
use serde::Deserialize;
use sun;
use tokio::{self,
            sync::mpsc::{self, Sender},
            time::{Duration, sleep}
};
use tokio::task::JoinHandle;
use decide_protocol::{Component, error::DecideError};

pub mod proto {
    include!(concat!(env!("OUT_DIR"), "/_.rs"));
}

pub struct HouseLight {
    switch: Arc<AtomicBool>,
    light_override: Arc<AtomicBool>,
    ephemera: Arc<AtomicBool>,
    brightness: Arc<AtomicU8>,
    interval: Arc<AtomicU32>,
    state_sender: mpsc::Sender<Any>,
    task_handle: Option<JoinHandle<()>>,
}

#[async_trait]
impl Component for HouseLight {
    type State = proto::State;
    type Params = proto::Params;
    type Config = Config;
    const STATE_TYPE_URL: &'static str = "melizalab.org/proto/house_light_state";
    const PARAMS_TYPE_URL: &'static str = "melizalab.org/proto/house_light_state";

    fn new(_config: Self::Config, state_sender: Sender<Any>) -> Self {
        HouseLight {
            switch: Arc::new(AtomicBool::new(true)),
            light_override: Arc::new(AtomicBool::new(false)),
            ephemera: Arc::new(AtomicBool::new(false)),
            brightness: Arc::new(AtomicU8::new(0)),
            interval: Arc::new(AtomicU32::new(300)),
            state_sender,
            task_handle: None,
        }
    }

    async fn init(&mut self, config: Self::Config) {
        let dev_path = std::fs::canonicalize(config.device_path).unwrap();
        let mut device = OpenOptions::new()
            .write(true)
            .read(true)
            .open(Path::new(&dev_path))
            .map_err(|e| DecideError::Component {source:e.into()})
            .unwrap();
        let dawn = config.fake_dawn;
        let dusk = config.fake_dusk;
        let max_brightness = config.max_brightness;

        let switch = self.switch.clone();
        let light_override = self.light_override.clone();
        let ephemera = self.ephemera.clone();
        let brightness = self.brightness.clone();
        let interval = self.interval.clone();
        let sender = self.state_sender.clone();

        self.task_handle = Some(tokio::spawn(async move{
            loop{
                if switch.load(Ordering::Relaxed) {
                    if !light_override.load(Ordering::Relaxed) {
                        let ephemera = ephemera.load(Ordering::Relaxed);
                        let altitude = HouseLight::calc_altitude(ephemera, dawn, dusk);
                        let new_brightness = HouseLight::calc_brightness(altitude, max_brightness);

                        let write_brightness = format!("{}", new_brightness);
                        device.write_all(&write_brightness.as_bytes())
                            .map_err(|e|DecideError::Component {source:e.into()})
                            .unwrap();
                        tracing::debug!("Brightness written to sysfs file");
                        brightness.store(new_brightness, Ordering::Relaxed);
                        let state = Self::State {
                            switch: true,
                            light_override: false,
                            ephemera,
                            brightness: new_brightness as i32
                        };
                        let message = Any {
                            value: state.encode_to_vec(),
                            type_url: Self::STATE_TYPE_URL.into(),
                        };
                        sender.send(message).await
                            .map_err(|e| DecideError::Component { source: e.into() })
                            .unwrap();
                    } else {
                        let new_brightness = brightness.load(Ordering::Relaxed);
                        let write_brightness = format!("{}", new_brightness);
                        device.write_all(&write_brightness.as_bytes())
                            .map_err(|e| DecideError::Component { source: e.into() })
                            .unwrap();
                        tracing::debug!("Manual brightness written to sysfs file");
                        brightness.store(new_brightness, Ordering::Relaxed);
                        let state = Self::State {
                            switch: true,
                            light_override: true,
                            ephemera: false,
                            brightness: new_brightness as i32
                        };
                        let message = Any {
                            value: state.encode_to_vec(),
                            type_url: Self::STATE_TYPE_URL.into(),
                        };
                        sender.send(message).await
                            .map_err(|e| DecideError::Component { source: e.into() })
                            .unwrap();
                    }
                }
                sleep(Duration::from_secs(interval.load(Ordering::Relaxed) as u64)).await;
            }
        }));
    }

    fn change_state(&mut self, state: Self::State) -> decide_protocol::Result<()> {
        let sender = self.state_sender.clone();

        self.switch.store(state.switch, Ordering::Relaxed);
        self.ephemera.store(state.ephemera, Ordering::Relaxed);
        self.brightness.store(state.brightness as u8, Ordering::Relaxed);

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
        self.interval.store(params.clock_interval as u32, Ordering::Relaxed);
        Ok(())
    }

    fn get_state(&self) -> Self::State {
        Self::State {
            switch: self.switch.load(Ordering::Relaxed),
            light_override: self.light_override.load(Ordering::Relaxed),
            ephemera: self.ephemera.load(Ordering::Relaxed),
            brightness: self.brightness.load(Ordering::Relaxed) as i32,
        }
    }

    fn get_parameters(&self) -> Self::Params {
        Self::Params {
            clock_interval: self.interval.load(Ordering::Relaxed) as i64
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
    fn calc_brightness(altitude: f64, max_brightness: u8) -> u8 {
        let x = (altitude.sin() * (max_brightness as f64)).round() as u8;
        //let brightness = max(0.0, x); //trait 'Ord' is not implemented for '{float}'
        let brightness = if x > 0 { x } else { 0 };
        brightness
    }
    fn calc_altitude(ephemera: bool, dawn: f64, dusk: f64) -> f64 {
        return if ephemera {
            let now: DateTime<Utc> = DateTime::from(SystemTime::now());
            let now = (now.hour() + (now.minute() / 60) + (now.second() / 3600)) as f64;
            let x: f64 = (now + 24.0 - dawn) % 24.0;
            let y: f64 = (dusk + 24.0 - now) % 24.0;
            let altitude = (x / y) * 3.14;
            altitude
        } else {
            let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
            let altitude = sun::pos(now.as_millis() as i64, 0.0, 0.0)
                .altitude;
            altitude
        }
    }
}

#[derive(Deserialize)]
pub struct Config {
    device_path: String, //device_path: "/sys/class/leds/starboard::lights/brightness".to_string(),
    fake_dawn: f64,
    fake_dusk: f64,
    max_brightness: u8,
}