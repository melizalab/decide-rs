use async_trait::async_trait;
use atomic_wait::wait;
use decide_protocol::{error::DecideError,
                      Component};
use hal::{delay, i2c};
use i2cdev::linux::LinuxI2CBus;
use linux_embedded_hal_async as hal;
use prost::Message;
use prost_types::Any;
use serde::Deserialize;
use std::sync::atomic::{AtomicU16, AtomicU32, Ordering};
use std::sync::{
    atomic::AtomicBool,
    Arc,
};
use tokio::{
    self, sync::mpsc, task::JoinHandle
};
use vl53l4cd::Vl53l4cd;

pub struct TripWire {
    polling: Arc<AtomicU32>,
    blocking: Arc<AtomicBool>,
    timing: Arc<[AtomicU32; 2]>,
    range: Arc<[AtomicU16; 2]>,
    state_sender: mpsc::Sender<Any>,
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
            polling: Arc::new(AtomicU32::new(0)),
            blocking: Arc::new(AtomicBool::new(false)),
            timing: Arc::new([
                AtomicU32::new(config.budget.clamp(10,200)),
                AtomicU32::new(config.interval)]),
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
        let obj_timing = Arc::clone(&self.timing);
        let obj_polling = Arc::clone(&self.polling);

        let trip_handle = tokio::spawn(async move{
            let dev = i2c::LinuxI2c::new(
                LinuxI2CBus::new(config.i2c_bus).unwrap()
            );
            let mut sensor = Vl53l4cd::new(
                dev,
                delay::LinuxDelay,
                vl53l4cd::wait::Poll
            );
            let range = obj_range.iter()
                .map(|r| r.load(Ordering::Relaxed))
                .collect::<Vec<u16>>();
            let timing = obj_timing.iter()
                .map(|t| t.load(Ordering::Relaxed))
                .collect::<Vec<u32>>();
            let mut blocking = false;

            sensor.init().await
                .map_err(|e| DecideError::Component {source: e.into() })
                .unwrap();
            sensor.set_range_timing(timing[0], timing[1]).await
                .map_err(|e| DecideError::Component {source: e.into() })
                .unwrap();

            loop {
                if shutdown_rx.try_recv().unwrap_err() == mpsc::error::TryRecvError::Disconnected {
                    break
                };
                wait(&obj_polling, 0);
                let measure = sensor.measure().await
                    .map_err(|e| DecideError::Component {source: e.into() })
                    .unwrap();
                if measure.is_valid() {
                    if (measure.distance>range[0])&(measure.distance<range[1])&(!blocking) {
                        Self::send_state(
                            &Self::State{polling: true, blocking: true},
                            &sender
                        ).await;
                        blocking=true
                    } else if blocking&((measure.distance<range[0])|(measure.distance>range[1])) {
                        Self::send_state(
                            &Self::State{polling: true, blocking: false},
                            &sender
                        ).await;
                        blocking=false
                    }
                } else {
                    tracing::debug!("Invalid measurement.")
                }
            }
        });
        self.shutdown = Some((trip_handle, shutdown_tx));
        tracing::info!("TripWire Initiation Complete.");
    }

    fn change_state(&mut self, state: Self::State) -> decide_protocol::Result<()> {
        match state.polling {
            true => { self.polling.store(1, Ordering::Release) }
            false => { self.polling.store(0, Ordering::Release) }
        };
        tracing::info!("TripWire: State Changed by Request");
        Ok(())
    }

    fn set_parameters(&mut self, _params: Self::Params) -> decide_protocol::Result<()> {
        tracing::error!("TripWire params is empty. Make sure your script isn't using it without good reason");
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
        tracing::debug!("Emitting state change");
        sender.send(Any {
            type_url: String::from(Self::STATE_TYPE_URL),
            value: state.encode_to_vec(),
        }).await.map_err(|e| DecideError::Component { source: e.into() }).unwrap();
    }

    async fn shutdown(&mut self) {
        tracing::info!("TripWire: Shutdown Called");
        if let Some((handle, sender)) = self.shutdown.take() {
            drop(sender);
            drop(handle);
        }
    }
}

pub mod proto {
    include!(concat!(env!("OUT_DIR"), "/_.rs"));
}

#[derive(Deserialize)]
pub struct Config {
    i2c_bus: String, // "/dev/i2c-2"
    // addr: u8,
    budget: u32,
    interval: u32,
    min_range: u16,
    max_range: u16,
}