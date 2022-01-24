use decide_proto::{Component, error::{DecideError, ComponentError}
};
use prost::Message;
use prost_types::Any;

use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
use serde::Deserialize;

use async_trait::async_trait;
use tokio::{self,
            sync::mpsc::{self, Sender},
            time::{sleep, Duration}
};

use std::time::{SystemTime, UNIX_EPOCH};
use chrono::{self, Utc, DateTime, Timelike};
use sun;

use std::fs::OpenOptions;
use std::io::{Write};
use std::path::Path;
use std::sync::atomic::{AtomicU32, AtomicU8};
use tokio::io::AsyncWriteExt;
use decide_proto::error::ComponentError::FileAccessError;

pub mod house_light {
    include!(concat!(env!("OUT_DIR"), "/_.rs"));
}

pub struct HouseLight {
    switch: Arc<AtomicBool>,
    fake_clock: Arc<AtomicBool>,
    brightness: Arc<AtomicU8>,
    interval: Arc<AtomicU32>,
    state_sender: Option<mpsc::Sender<Any>>,
}

#[async_trait]
impl Component for HouseLight {
    type State = house_light::State;
    type Params = house_light::Params;
    type Config = Config;
    const STATE_TYPE_URL: &'static str = "melizalab.org/proto/house_light_state";
    const PARAMS_TYPE_URL: &'static str = "melizalab.org/proto/house_light_state";

    fn new(_config: Self::Config) -> Self {
        HouseLight {
            switch: Arc::new(AtomicBool::new(false)),
            fake_clock: Arc::new(AtomicBool::new(false)),
            brightness: Arc::new(AtomicU8::new(0)),
            interval: Arc::new(AtomicU32::new(300)),
            state_sender: None,
        }
    }

    async fn init(&mut self, config: Self::Config, state_sender: Sender<Any>) {
        self.state_sender = Some(state_sender.clone());
        let switch = self.switch.clone();
        let fake_clock = self.fake_clock.clone();
        let brightness = self.brightness.clone();
        let interval = self.interval.clone();

        tokio::spawn(async move {
            let mut device = OpenOptions::new()
                .write(true)
                .read(true)
                .open(Path::new(&config.device_path))
                .map_err(|e| FileAccessError {source:e, dir: &config.device_path.clone()})
                .unwrap();
            let dawn = config.fake_dawn;
            let dusk = config.fake_dusk;
            let max_brightness = config.max_brightness;
            loop {
                if switch.load(Ordering::Relaxed) {
                    let fake_clock = fake_clock.load(Ordering::Relaxed);
                    let altitude = HouseLight::calc_altitude(fake_clock, dawn, dusk);
                    let new_brightness = HouseLight::calc_brightness(altitude, max_brightness);

                    device.write(&[new_brightness]).unwrap();
                    tracing::trace!("Brightness written to sysfs file");
                    brightness.store(new_brightness, Ordering::Relaxed);

                    let state = Self::State {
                        switch: true,
                        fake_clock,
                        brightness: new_brightness as i32
                    };
                    let message = Any {
                        value: state.encode_to_vec(),
                        type_url: Self::STATE_TYPE_URL.into(),
                    };
                    state_sender.send(message).await.unwrap();
                }
                sleep(Duration::from_secs(interval.load(Ordering::Relaxed) as u64)).await;
            }
        });
    }

    fn change_state(&mut self, state: Self::State) -> decide_proto::Result<()> {
        let sender = self.state_sender.as_mut().cloned().unwrap();

        self.switch.store(state.switch, Ordering::Relaxed);
        self.fake_clock.store(state.fake_clock, Ordering::Relaxed);
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
            tracing::trace!("state changed");
        });

        Ok(())
    }

    fn set_parameters(&mut self, params: Self::Params) -> decide_proto::Result<()> {
        self.interval.store(params.clock_interval as u32, Ordering::Relaxed);
        Ok(())
    }

    fn get_state(&self) -> Self::State {
        Self::State {
            switch: self.switch.load(Ordering::Relaxed),
            fake_clock: self.fake_clock.load(Ordering::Relaxed),
            brightness: self.brightness.load(Ordering::Relaxed) as i32,
        }
    }

    fn get_parameters(&self) -> Self::Params {
        Self::Params {
            clock_interval: self.interval.load(Ordering::Relaxed) as i64
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
    fn calc_altitude(fake_clock: bool, dawn: f64, dusk: f64) -> f64 {
        return if fake_clock {
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
    //latitude: f64,
    //longitude: f64,
}