use decide_protocol::{Component,
                      error::DecideError};
use prost::Message;
use prost_types::Any;
use serde::Deserialize;
use std::sync::{
    atomic::{AtomicBool},
    Arc,
};
use std::sync::atomic::{AtomicU8, AtomicU16, Ordering};
use tokio::{
    self, sync::mpsc, task::JoinHandle
};
use async_trait::async_trait;
use vl53l4cd::Vl53l4cd;
use linux_embedded_hal_async as hal;
use i2cdev::linux::LinuxI2CBus;
use hal::{delay, i2c};

pub struct TripWire {
    blocking: Arc<AtomicBool>,
    range: Arc<[AtomicU16; 2]>,
    state_sender: mpsc::Sender<Any>,
    req_sender: Option<mpsc::Sender<[bool; 2]>>,
    shutdown: Option<(JoinHandle<()>,
                      mpsc::Sender<bool>)>
}

#[async_trait]
impl Component for TripWire {
    type State = proto::WireState;
    type Params = proto::WireParams;
    type Config = Config;
    const STATE_TYPE_URL: &'static str = "type.googleapis.com/TripState";
    const PARAMS_TYPE_URL: &'static str = "type.googleapis.com/TripParams";

    fn new(config: Self::Config, state_sender: mpsc::Sender<Any>) -> Self{
        TripWire {
            blocking: Arc::new(AtomicBool::new(false)),
            range: Arc::new([
                AtomicU16::new(config.min_range),
                AtomicU16::new(config.max_range)]),
            state_sender,
            req_sender: None,
            shutdown: None,
        }
    }

    async fn init(&mut self, config: Self::Config) {

        let (req_snd, mut req_rcv) = mpsc::channel(20);
        let (shutdown_tx, mut shutdown_rx) = mpsc::channel(2);
        self.req_sender = Some(req_snd);

        let trip_handle = tokio::spawn(async move{
            let dev = i2c::LinuxI2c::new(
                LinuxI2CBus::new(config.i2c_bus).unwrap()
            );
            let mut sensor = Vl53l4cd::new(
                dev,
                delay::LinuxDelay,
                vl53l4cd::wait::Poll
            );
            sensor.init().await
                .map_err(|e| DecideError::Component {source: e.into() })
                .unwrap();
            sensor.start_ranging().await
                .map_err(|e| DecideError::Component {source: e.into() })
                .unwrap();

            loop {
                if shutdown_rx.try_recv().unwrap_err() == mpsc::error::TryRecvError::Disconnected {
                    break
                };


            }
        });
        self.shutdown = Some((trip_handle, shutdown_tx));
        tracing::info!("TripWire Initiation Complete.");
    }

    fn change_state(&mut self, state: Self::State) -> decide_protocol::Result<()> {
        todo!()
    }

    fn set_parameters(&mut self, params: Self::Params) -> decide_protocol::Result<()> {
        todo!()
    }

    fn get_state(&self) -> Self::State {
        todo!()
    }

    fn get_parameters(&self) -> Self::Params {
        todo!()
    }

    async fn shutdown(&mut self) {
        todo!()
    }
}

pub mod proto {
    include!(concat!(env!("OUT_DIR"), "/_.rs"));
}

#[derive(Deserialize)]
pub struct Config {
    i2c_bus: String, // "/dev/i2c-2"
    // addr: u8,
    trigger_time: u32,
    min_range: u16,
    max_range: u16,
}