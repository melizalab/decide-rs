use thiserror::Error;
use prost::Message;
use prost_types::Any;
use serde::Deserialize;

use async_trait::async_trait;
use atomic_wait::{wait, wake_one};
use std::sync::atomic::{AtomicU16, AtomicU32, Ordering};
use std::sync::{atomic::AtomicBool, Arc};
use tokio::{self, sync::mpsc, task::JoinHandle};
use decide_protocol::{error::DecideError, Component};

use simple_moving_average::{SMA, NoSumSMA};
use vl53l4cd::Vl53l4cd;
use vcnl4040::{LedCurrent, LedDutyCycle, ProximityIntegrationTime, Vcnl4040};
use hal::{delay, i2c};
use i2cdev::linux::LinuxI2CBus;
use linux_embedded_hal_async as hal;

pub struct TofSensor {
    polling: Arc<AtomicU32>,
    blocking: Arc<AtomicBool>,
    range: Arc<[AtomicU16; 2]>,
    state_sender: mpsc::Sender<Any>,
    shutdown: Option<(JoinHandle<()>,
                      mpsc::Sender<bool>)>
}

pub struct Prox4040Sensor {
    polling: Arc<AtomicU32>,
    blocking: Arc<AtomicBool>,
    range: Arc<[AtomicU16; 2]>,
    state_sender: mpsc::Sender<Any>,
    shutdown: Option<(JoinHandle<()>,
                      mpsc::Sender<bool>)>
}

#[async_trait]
impl Component for TofSensor {
    type State = proto::TofState;
    type Params = proto::TofParams;
    type Config = Config;
    const STATE_TYPE_URL: &'static str = "type.googleapis.com/TofState";
    const PARAMS_TYPE_URL: &'static str = "type.googleapis.com/TofParams";

    fn new(config: Self::Config, state_sender: mpsc::Sender<Any>) -> Self{
        TofSensor {
            polling: Arc::new(AtomicU32::new(1)),
            blocking: Arc::new(AtomicBool::new(false)),
            range: Arc::new([
                AtomicU16::new(config.min_range),
                AtomicU16::new(config.max_range)]),
            state_sender,
            shutdown: None,
        }
    }

    async fn init(&mut self, config: Self::Config) {

        let (shutdown_tx, mut shutdown_rx) = mpsc::channel(2);
        let sender = self.state_sender.clone();

        let obj_range = Arc::clone(&self.range);
        let obj_polling = Arc::clone(&self.polling);

        let trip_handle = tokio::spawn(async move{
            let dev = i2c::LinuxI2c::new(
                LinuxI2CBus::new(config.i2c_bus.clone())
                    .map_err(|_e| DecideError::Component { source:
                        TofError::InvalidFs{path: config.i2c_bus.clone()}.into()
                    }).unwrap()
            );
            let mut sensor = Vl53l4cd::new(
                dev,
                delay::LinuxDelay,
                vl53l4cd::wait::Poll
            );
            let range = obj_range.iter()
                .map(|r| r.load(Ordering::Relaxed))
                .collect::<Vec<u16>>();
            let timing = vec![
                config.budget.clamp(10, 200),
                config.interval
            ];
            let mut blocking = false;

            sensor.init().await
                .map_err(|_e| DecideError::Component {source:
                    TofError::I2CError {tag:"init".to_string()}.into() })
                .unwrap();
            sensor.set_range_timing(timing[0], timing[1]).await
                .map_err(|_e| DecideError::Component {source:
                    TofError::I2CError {tag:"set_range_timing".to_string()}.into() })
                .unwrap();
            let mut rolling_range = NoSumSMA::<_, u16, 5>::new();
            let mut mean_range: u16;
            let mut error_count: u8 = 0;
            'engine: loop {
                if shutdown_rx.try_recv().unwrap_err() == mpsc::error::TryRecvError::Disconnected {
                    break 'engine
                };
                wait(&obj_polling, 0);
                sensor.start_ranging().await.unwrap();
                'measure: while obj_polling.load(Ordering::Acquire)==1 {
                    match sensor.measure().await {
                        Err(_e) => {
                            tracing::warn!("measure invalid! {_e}");
                            if error_count+1 > 20 {
                                panic!("{}", &TofError::MeasureError);
                            } else {
                                error_count +=1;
                                continue 'measure
                            }
                        }
                        Ok(measure) => {
                            if measure.is_valid() {
                                error_count = 0;
                                rolling_range.add_sample(measure.distance);
                                mean_range = rolling_range.get_average();
                                if (mean_range > range[0]) & (mean_range < range[1]) & (!blocking) {
                                    tracing::info!("rangefinder blocked!");
                                    Self::send_state(
                                        &Self::State { polling: true, blocking: true }, &sender).await;
                                    blocking = true
                                } else if blocking & ((mean_range < range[0]) | (mean_range > range[1])) {
                                    tracing::info!("rangefinder unblocked!");
                                    Self::send_state(
                                        &Self::State { polling: true, blocking: false }, &sender).await;
                                    blocking = false
                                }
                            }
                        }
                    }
                };
                sensor.stop_ranging().await.unwrap();
            }
        });
        self.shutdown = Some((trip_handle, shutdown_tx));
        tracing::info!("rangefinder vl53l4cd initiated.");
    }

    fn change_state(&mut self, state: Self::State) -> decide_protocol::Result<()> {
        match state.polling {
            true => { self.polling.store(1, Ordering::Release) }
            false => { self.polling.store(0, Ordering::Release) }
        };
        wake_one(self.polling.as_ref());
        Ok(())
    }

    fn set_parameters(&mut self, _params: Self::Params) -> decide_protocol::Result<()> {
        tracing::error!("rangefinder vl53l4cd params is empty.");
        Ok(())
    }

    fn get_state(&self) -> Self::State {
        let polling: bool = match self.polling.load(Ordering::Relaxed) {
            1 => true,
            _ => false,
        };
        Self::State {
            polling,
            blocking: self.blocking.load(Ordering::Relaxed)
        }
    }

    fn get_parameters(&self) -> Self::Params {
        Self::Params {}
    }

    async fn send_state(state: &Self::State, sender: &mpsc::Sender<Any>) {
        sender.send(Any {
            type_url: String::from(Self::STATE_TYPE_URL),
            value: state.encode_to_vec(),
        }).await.map_err(|_e| DecideError::Component { source:
            TofError::SendError.into() }).unwrap();
    }

    async fn shutdown(&mut self) {
        if let Some((handle, sender)) = self.shutdown.take() {
            drop(sender);
            handle.abort();
            handle.await.unwrap_err();
        }
    }
}

#[async_trait]
impl Component for Prox4040Sensor {
    type State = proto::TofState;
    type Params = proto::TofParams;
    type Config = Config;
    const STATE_TYPE_URL: &'static str = "type.googleapis.com/TofState";
    const PARAMS_TYPE_URL: &'static str = "type.googleapis.com/TofParams";

    fn new(config: Self::Config, state_sender: mpsc::Sender<Any>) -> Self{
        ProxSensor {
            polling: Arc::new(AtomicU32::new(1)),
            blocking: Arc::new(AtomicBool::new(false)),
            range: Arc::new([
                AtomicU16::new(config.min_range),
                AtomicU16::new(config.max_range)]),
            state_sender,
            shutdown: None,
        }
    }

    async fn init(&mut self, config: Self::Config) {

        let (shutdown_tx, mut shutdown_rx) = mpsc::channel(2);
        let sender = self.state_sender.clone();

        let obj_range = Arc::clone(&self.range);
        let obj_polling = Arc::clone(&self.polling);

        let trip_handle = tokio::spawn(async move{
            let dev = i2c::LinuxI2c::new(
                LinuxI2CBus::new(config.i2c_bus.clone())
                    .map_err(|_e| DecideError::Component { source:
                        TofError::InvalidFs{path: config.i2c_bus.clone()}.into()
                    }).unwrap()
            );
            let mut sensor = Vcnl4040::new(dev);

            let range = obj_range.iter()
                .map(|r| r.load(Ordering::Relaxed))
                .collect::<Vec<u16>>();
            let mut blocking = false;

            sensor.init(false).await
                .map_err(|_e| DecideError::Component {source:
                    TofError::I2CError {tag:"init".to_string()}.into() })
                .unwrap();
            sensor.enable_proximity(true).await
                .map_err(|_e| DecideError::Component {source:
                    TofError::I2CError {tag:"init".to_string()}.into() })
                .unwrap();

            sensor.set_proximity_led_current(LedCurrent::Current100mA).await
                .map_err(|_e| DecideError::Component {source:
                    TofError::I2CError {tag:"set current".to_string()}.into() })
                .unwrap();
            sensor.set_proximity_led_duty_cycle(LedDutyCycle::Duty1_160).await
                .map_err(|_e| DecideError::Component {source:
                    TofError::I2CError {tag:"set duty cycle".to_string()}.into() })
                .unwrap();
            sensor.set_proximity_integration_time(ProximityIntegrationTime::Time2T).await
                .map_err(|_e| DecideError::Component {source:
                    TofError::I2CError {tag:"set integration time".to_string()}.into() })
                .unwrap();

            let mut rolling_range = NoSumSMA::<_, u16, 5>::new();
            let mut mean_range: u16;

            loop {
                if shutdown_rx.try_recv().unwrap_err() == mpsc::error::TryRecvError::Disconnected {
                    break
                };
                wait(&obj_polling, 0);
                while obj_polling.load(Ordering::Acquire)==1 {
                    let distance = sensor.get_proximity().await.unwrap();
                    rolling_range.add_sample(distance);
                    mean_range = rolling_range.get_average();
                    if (mean_range > range[0]) & (mean_range < range[1]) & (!blocking) {
                        tracing::info!("rangefinder blocked!");
                        Self::send_state(
                            &Self::State { polling: true, blocking: true },
                            &sender
                        ).await;
                        blocking = true
                    } else if blocking & ((mean_range < range[0]) | (mean_range > range[1])) {
                        tracing::info!("rangefinder unblocked!");
                        Self::send_state(
                            &Self::State { polling: true, blocking: false },
                            &sender
                        ).await;
                        blocking = false
                    }
                };
            }
        });
        self.shutdown = Some((trip_handle, shutdown_tx));
        tracing::info!("rangefinder vcnl4040 initiated.");
    }

    fn change_state(&mut self, state: Self::State) -> decide_protocol::Result<()> {
        match state.polling {
            true => { self.polling.store(1, Ordering::Release) }
            false => { self.polling.store(0, Ordering::Release) }
        };
        // block_on(
        //     Self::send_state(&Self::State { polling: true, blocking: false }, &self.state_sender.clone())
        // );
        wake_one(self.polling.as_ref());
        Ok(())
    }

    fn set_parameters(&mut self, _params: Self::Params) -> decide_protocol::Result<()> {
        tracing::error!("rangefinder vcnl4040 params is empty.");
        Ok(())
    }

    fn get_state(&self) -> Self::State {
        let polling: bool = match self.polling.load(Ordering::Relaxed) {
            1 => true,
            _ => false,
        };
        Self::State {
            polling,
            blocking: self.blocking.load(Ordering::Relaxed)
        }
    }

    fn get_parameters(&self) -> Self::Params {
        Self::Params {}
    }

    async fn send_state(state: &Self::State, sender: &mpsc::Sender<Any>) {
        sender.send(Any {
            type_url: String::from(Self::STATE_TYPE_URL),
            value: state.encode_to_vec(),
        }).await.map_err(|_e| DecideError::Component { source:
        TripWireError::SendError.into() }).unwrap();
    }

    async fn shutdown(&mut self) {
        if let Some((handle, sender)) = self.shutdown.take() {
            drop(sender);
            handle.abort();
            handle.await.unwrap_err();
        }
    }
}


pub mod proto {
    include!(concat!(env!("OUT_DIR"), "/_.rs"));
}

#[derive(Deserialize)]
pub struct Config {
    i2c_bus: String, // "/dev/i2c-2"
    budget: u32,
    interval: u32,
    min_range: u16,
    max_range: u16,
}

#[derive(Error, Debug)]
pub enum TofError {
    #[error("could not access file {path:?}")]
    InvalidFs{path: String},
    #[error("error accessing I2C device for {tag:?}")]
    I2CError{tag: String},
    #[error("too many invalid readings from device.")]
    MeasureError,
    #[error("could not send state update")]
    SendError,
}