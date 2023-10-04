use std::sync::{Arc, atomic::{AtomicBool, AtomicU64, Ordering}};
use std::time::Instant;
use async_trait::async_trait;
use futures::stream::StreamExt;
use gpio_cdev::{AsyncLineEventHandle, Chip,
                EventRequestFlags,
                EventType,
                LineRequestFlags,
                MultiLineHandle};
use prost::Message;
use prost_types::Any;
use serde::Deserialize;
use tokio::{self, time::Duration, sync::mpsc};
use decide_protocol::{Component, error::DecideError};

pub struct StepperMotor {
    running: Arc<AtomicBool>,
    direction: Arc<AtomicBool>,
    timeout: Arc<AtomicU64>,
    state_sender: mpsc::Sender<Any>,
    req_sender: Option<mpsc::Sender<[bool; 2]>>,
    shutdown: Option<(tokio::task::JoinHandle<()>,
                      mpsc::Sender<bool>)>
}

#[async_trait]
impl Component for StepperMotor {
    type State = proto::SmState;
    type Params = proto::SmParams;
    type Config = Config;
    const STATE_TYPE_URL: &'static str = "type.googleapis.com/SmState";
    const PARAMS_TYPE_URL: &'static str = "type.googleapis.com/SmParams";

    fn new(_config: Self::Config, state_sender: mpsc::Sender<Any>) -> Self {
        use std::fs;
        use std::path::Path;

        if Path::new("/sys/class/pwm/pwmchip5").exists() {
            if !Path::new("/sys/class/pwm/pwmchip5/pwm0").exists() {
                fs::write("/sys/class/pwm/pwmchip5/export", "0").expect("Unable to export pwmchip5/pwm0");
            }
            if !Path::new("/sys/class/pwm/pwmchip5/pwm1").exists() {
                fs::write("/sys/class/pwm/pwmchip5/export", "1").expect("Unable to export pwmchip5/pwm1");
            }
            fs::write("/sys/class/pwm/pwmchip5/pwm0/period", "10000").expect("Unable to write to pwm0 period");
            fs::write("/sys/class/pwm/pwmchip5/pwm1/period", "10000").expect("Unable to write to pwm1 period");
            fs::write("/sys/class/pwm/pwmchip5/pwm0/duty_cycle", "6500").expect("Unable to write to pwm0 duty_cycle");
            fs::write("/sys/class/pwm/pwmchip5/pwm1/duty_cycle", "6500").expect("Unable to write to pwm1 duty_cycle");
            fs::write("/sys/class/pwm/pwmchip5/pwm0/enable", "1").expect("Unable to write to pwm0 enable");
            fs::write("/sys/class/pwm/pwmchip5/pwm1/enable", "1").expect("Unable to write to pwm1 enable");
        } else if Path::new("/sys/class/pwm/pwmchip0").exists() {
            if !Path::new("/sys/class/pwm/pwmchip0/pwm0").exists() {
                fs::write("/sys/class/pwm/pwmchip0/export", "0").expect("Unable to export pwmchip0/pwm0");
            }
            if !Path::new("/sys/class/pwm/pwmchip0/pwm1").exists() {
                fs::write("/sys/class/pwm/pwmchip0/export", "1").expect("Unable to export pwmchip0/pwm1");
            }
            fs::write("/sys/class/pwm/pwmchip0/pwm0/period", "10000").expect("Unable to write to pwm0 period");
            fs::write("/sys/class/pwm/pwmchip0/pwm1/period", "10000").expect("Unable to write to pwm1 period");
            fs::write("/sys/class/pwm/pwmchip0/pwm0/duty_cycle", "6500").expect("Unable to write to pwm0 duty_cycle");
            fs::write("/sys/class/pwm/pwmchip0/pwm1/duty_cycle", "6500").expect("Unable to write to pwm1 duty_cycle");
            fs::write("/sys/class/pwm/pwmchip0/pwm0/enable", "1").expect("Unable to write to pwm0 enable");
            fs::write("/sys/class/pwm/pwmchip0/pwm1/enable", "1").expect("Unable to write to pwm1 enable");
        } else {
            tracing::error!("Found neither pwmchip0 nor pwmchip5 for stepper motor");
            panic!("stepper motor pwmchip not found :(")
        }

        StepperMotor {
            running: Arc::new(AtomicBool::new(false)),
            direction: Arc::new(AtomicBool::new(true)),
            timeout: Arc::new(AtomicU64::new(500)),
            state_sender,
            req_sender: None,
            shutdown: None,
        }
    }

    async fn init(&mut self, config: Self::Config) {

        let (req_snd, mut req_rcv) = mpsc::channel(20);
        self.req_sender = Some(req_snd);
        let mut chip1 = Chip::new(config.chip1.clone())
            .map_err(|e| DecideError::Component { source: e.into() })
            .unwrap();
        let mut chip3 = Chip::new(config.chip3.clone())
            .map_err(|e| DecideError::Component { source: e.into() })
            .unwrap();
        let motor_1_handle = StepperMotor::request_lines(&mut chip1, &config.motor1_offsets);
        let motor_3_handle = StepperMotor::request_lines(&mut chip3, &config.motor3_offsets);
        let mut switch_14 = StepperMotor::request_asynclines(&mut chip1, config.switch_offsets[0]);
        let mut switch_15 = StepperMotor::request_asynclines(&mut chip1, config.switch_offsets[1]);

        let running = self.running.clone();
        let direction = self.direction.clone();
        let mut state_sender = self.state_sender.clone();
        let timeout = Arc::clone(&self.timeout);
        let (shutdown_tx, mut shutdown_rx) = mpsc::channel(1);
        let dt = config.dt;

        let motor_handle = tokio::spawn(async move {
            let mut step = 0;
            loop {
                if shutdown_rx.try_recv().unwrap_err() == mpsc::error::TryRecvError::Disconnected {
                    StepperMotor::pause_motor(&motor_1_handle, &motor_3_handle);
                    break}

                let mut state = StepperMotor::poll_change(&mut switch_14,
                                                      &mut switch_15,
                                                      &mut req_rcv).await;
                if state.running {
                    running.store(state.running, Ordering::Release);
                    direction.store(state.direction, Ordering::Release);
                    tracing::debug!("sending state");
                    StepperMotor::send_state(&state, &mut state_sender).await;
                    tracing::debug!("Running motor with timeout");
                    let timer = Instant::now();
                    while Instant::now().duration_since(timer) <
                        Duration::from_millis(timeout.load(Ordering::Acquire)) {
                        step = StepperMotor::run_motor(step, &motor_1_handle,
                                                       &motor_3_handle, state.direction);
                        tokio::time::sleep(Duration::from_micros(dt)).await;
                    }
                    StepperMotor::pause_motor(&motor_1_handle, &motor_3_handle);
                    state.running = false;
                    running.store(state.running, Ordering::Release);
                    direction.store(state.direction, Ordering::Release);
                    tracing::debug!("sending state");
                    StepperMotor::send_state(&state, &mut state_sender).await;
                } else {
                    tracing::debug!("Motor state poller triggered but not runned.");
                    tokio::time::sleep(Duration::from_micros(dt)).await;
                }
            }
        });
        self.shutdown = Some((motor_handle, shutdown_tx));
        tracing::info!("Stepper Motor Initiated");
    }

    fn change_state(&mut self, state: Self::State) -> decide_protocol::Result<()> {
        self.direction.store(state.direction, Ordering::Release);
        self.running.store(state.running, Ordering::Release);

        let notify = self.req_sender.clone();
        if state.running {
            tokio::spawn(async move {
                notify
                    .unwrap()
                    .send([state.running, state.direction])
                    .await
                    .map_err(|e| DecideError::Component { source: e.into() })
                    .unwrap();
            });
        }
        tracing::info!("Stepper Motor State Changed by Request");
        Ok(())
    }

    fn set_parameters(&mut self, params: Self::Params) -> decide_protocol::Result<()> {
        self.timeout.store(params.timeout, Ordering::Release);
        Ok(())
    }

    fn get_state(&self) -> Self::State {
        Self::State {
            running: self.running.load(Ordering::Acquire),
            direction: self.direction.load(Ordering::Acquire)
        }
    }

    fn get_parameters(&self) -> Self::Params {
        Self::Params {
            timeout: self.timeout.load(Ordering::Acquire)
        }
    }

    async fn shutdown(&mut self) {
        if let Some((motor_handle, sd_tx)) = self.shutdown.take() {
            drop(sd_tx);
            motor_handle.abort();
            motor_handle.await.unwrap_err();
        }
    }
}

struct LinesVal([u8; 2]);

impl StepperMotor {
    const NUM_HALF_STEPS: usize = 8;
    const ALL_OFF: LinesVal = LinesVal([0, 0]);
    const HALF_STEPS: [(LinesVal, LinesVal); 8] = [
        (LinesVal([0, 1]), LinesVal([1, 0])),
        (LinesVal([0, 1]), LinesVal([0, 0])),
        (LinesVal([0, 1]), LinesVal([0, 1])),
        (LinesVal([0, 0]), LinesVal([0, 1])),
        (LinesVal([1, 0]), LinesVal([0, 1])),
        (LinesVal([1, 0]), LinesVal([0, 0])),
        (LinesVal([1, 0]), LinesVal([1, 0])),
        (LinesVal([0, 0]), LinesVal([1, 0]))
    ];

    fn request_lines(chip: &mut Chip, lines: &[u32]) -> MultiLineHandle {
        return chip
            .get_lines(lines)
            .map_err(|e| DecideError::Component { source: e.into() }).unwrap()
            .request(LineRequestFlags::OUTPUT, &[0, 0], "decide-rs")
            .map_err(|e| DecideError::Component { source: e.into() }).unwrap();
    }

    fn request_asynclines(chip: &mut Chip, lines: u32) -> AsyncLineEventHandle {
        let line = chip.get_line(lines)
            .map_err(|e| DecideError::Component { source: e.into() })
            .unwrap();
        return AsyncLineEventHandle::new(
            line.events(
                LineRequestFlags::INPUT,
                EventRequestFlags::BOTH_EDGES,
                "decide-rs"
            ).map_err(|e| DecideError::Component { source: e.into() }).unwrap()
        ).map_err(|e| DecideError::Component { source: e.into() }).unwrap();

    }

    async fn poll_change(sw14: &mut AsyncLineEventHandle,
                         sw15: &mut AsyncLineEventHandle,
                         state_rx: &mut mpsc::Receiver<[bool; 2]>) -> proto::SmState {
        let mut state = proto::SmState {running: false, direction: false};
        tokio::select! {

            Some(event) = sw14.next() => {
                let evt_type = event.map_err(|e| DecideError::Component { source: e.into() })
                                    .unwrap().event_type();
                match evt_type {
                    EventType::RisingEdge => {
                        tracing::info!("Motor Switch 14 Pressed");
                        //state.running = false;
                        //state.direction = false;
                    }
                    EventType::FallingEdge => {
                        tracing::debug!("Motor Switch 14 Depressed");
                        state.running = true;
                        //state.direction = false;

                    }
                }
            }
            Some(event) = sw15.next() => {
                let evt_type = event.map_err(|e| DecideError::Component { source: e.into() })
                                    .unwrap().event_type();
                match evt_type {
                    EventType::RisingEdge => {
                        tracing::debug!("Motor Switch 15 Pressed");
                        //state.running = false;
                        state.direction = true;
                    }
                    EventType::FallingEdge => {
                        tracing::debug!("Motor Switch 15 Depressed");
                        state.running = true;
                        state.direction = true;

                    }
                }
            }
            Some(event) = state_rx.recv() => {
                if event.len() != 2 {
                    tracing::error!("Motor event poll received incorrect message");
                } else {
                    state.running = event[0];
                    state.direction = event[1];
                }
            }
        }
        return state
    }

    fn run_motor(mut step: usize, handle1: &MultiLineHandle, handle3: &MultiLineHandle, direction: bool) -> usize{
        if direction {
            step = (step + 1) % Self::NUM_HALF_STEPS;
            let step_1_values = &Self::HALF_STEPS[step].0;
            let step_3_values = &Self::HALF_STEPS[step].1;
            handle1.set_values(&step_1_values.0)
                .map_err(|e| DecideError::Component { source: e.into() })
                .unwrap();
            handle3.set_values(&step_3_values.0)
                .map_err(|e| DecideError::Component { source: e.into() })
                .unwrap();
        } else {
            step = (step - 1) % Self::NUM_HALF_STEPS;
            let step_1_values = &Self::HALF_STEPS[step].0;
            let step_3_values = &Self::HALF_STEPS[step].1;
            handle1.set_values(&step_1_values.0)
                .map_err(|e| DecideError::Component { source: e.into() })
                .unwrap();
            handle3.set_values(&step_3_values.0)
                .map_err(|e| DecideError::Component { source: e.into() })
                .unwrap();
        }
        step
    }

    fn pause_motor(handle1: &MultiLineHandle, handle3: &MultiLineHandle) {
        let step_1_values = &Self::ALL_OFF;
        let step_3_values = &Self::ALL_OFF;
        handle1.set_values(&step_1_values.0)
            .map_err(|e| DecideError::Component { source: e.into() })
            .unwrap();
        handle3.set_values(&step_3_values.0)
            .map_err(|e| DecideError::Component { source: e.into() })
            .unwrap();
    }

    async fn send_state(state: &proto::SmState, sender: &mut mpsc::Sender<Any>) {
        tracing::debug!("Emiting state change");
        sender.send(Any {
            type_url: String::from(Self::STATE_TYPE_URL),
            value: state.encode_to_vec(),
        }).await.map_err(|e| DecideError::Component { source: e.into() }).unwrap();
    }

}
pub mod proto {
    include!(concat!(env!("OUT_DIR"), "/_.rs"));
}

#[derive(Deserialize)]
pub struct Config {
    chip1: String, //"/dev/gpiochip1"
    chip3: String, //"/dev/gpiochip3"
    switch_offsets: [u32; 2], //14,15
    motor1_offsets: [u32; 2], //13, 12
    motor3_offsets: [u32; 2], //19,21
    dt: u64, //2000
}