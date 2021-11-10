use decide_proto::{Component, DecideError};
use async_trait::async_trait;
use prost_types::Any;
use tokio::{self, sync::mpsc, time::{sleep, Duration}};
use prost::Message;
use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
use serde::Deserialize;

use std::{fs, thread};
use std::time::{SystemTime, UNIX_EPOCH};
use sun_times;
use chrono;
use chrono::{Utc, DateTime, Timelike};
use std::cmp::{min,max};
use sun;
use std::fs::OpenOptions;
use std::io::{self,prelude::*,Write, Read};
use std::sync::atomic::AtomicU8;
use tokio::sync::mpsc::Sender;

pub struct HouseLight {
    switch: Arc<AtomicBool>,
    fake_clock: Arc<AtomicBool>,
    brightness: Arc<AtomicU8>,
    auto_update: bool,
    //daytime: AtomicBool,
}

#[async_trait]
impl Component for HouseLight {
    type State = house_light::State;
    type Params = house_light::Params;
    type Config = Config;
    const STATE_TYPE_URL: &'static str = "melizalab.org/proto/house_light_state";
    const PARAMS_TYPE_URL: &'static str = "melizalab.org/proto/house_light_state";

    fn new() -> Self {
        HouseLight {
            switch: Arc::new(AtomicBool::new(false)),
            fake_clock: Arc::new(AtomicBool::new(false)),
            brightness: Arc::new(AtomicU8::new(0)),
            auto_update: false
        }
    }

    fn init(&self, config: Self::Config, state_sender: Sender<Any>) {
        let switch = self.switch.clone();
        let fake_clock = self.fake_clock.clone();
        let brightness = self.brightness.clone();

        tokio::spawn(async move {
            let mut device = OpenOptions::new()
                .write(true)
                .read(true)
                .open(config.device_path).unwrap();
            loop {
                if switch.load(Ordering::Relaxed) {
                    let ephemera = fake_clock.load(Ordering::Relaxed);
                    let altitude = calc_altitude(ephemera, &config);
                    let new_brightness = calc_brightness(altitude, config.max_brightness);

                    device.write(&[news_brightness]).unwrap();

                    brightness.store(new_brightness, Ordering::Relaxed);
                    let state = Self::State {
                        switch: true,
                        ephemera,
                        new_brightness
                    };
                    let message = Any {
                        value: state.encode_to_vec(),
                        type_url: Self::STATE_TYPE_URL.into(),
                    };
                    sender.send(message).await.unwrap();
                }
                sleep(Duration::from_secs(300));
            }
        });

    }

    fn change_state(&mut self, state: Self::State) -> decide_proto::Result<()> {
        self.switch.store(state.switch, Ordering::Relaxed);
        self.fake_clock.store(state.fake_clock, Ordering::Relaxed);
        self.brightness.store(state.brightness, Ordering::Relaxed);
        Ok(())
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

fn calc_brightness (altitude: f64, max_brightness: u8) -> u8 {
    let brightness = max(0.0,(altitude.sin() * (max_brightness as f64)).round());
    brightness as u8
}
fn calc_altitude(fake_clock: bool, config: &HouseLight::Config) -> f64 {
    let mut altitude: f64 = 0.0;
    if fake_clock {
        let now: DateTime<Utc> = DateTime::from(SystemTime::now());
        let now= (now.hour() + (now.minute() / 60) + (now.second() / 3600)) as f64;
        let x: f64  = (now + 24 - config.fake_dawn) % 24;
        let y: f64 = (config.fake_dusk + 24 - now) % 24;
        altitude: f64 = (x/y) * 3.14;
    } else {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
        altitude = sun::pos(now.as_millis() as i64,0.0, 0.0)
            .altitude;
    };
    altitude
}

#[derive(Deserialize)]
struct Config {
    device_path: String, //device_path: "/sys/class/leds/starboard::lights/brightness".to_string(),
    fake_dawn: f64,
    fake_dusk: f64,
    max_brightness: u8,
    latitude: f64,
    longitude: f64,
}
