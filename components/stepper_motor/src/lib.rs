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
use thiserror::Error;
use tokio::{self, time::Duration, sync::mpsc};
use decide_protocol::{Component, error::DecideError};


pub struct StepperMotor {
    running: Arc<AtomicBool>,
    direction: Arc<AtomicBool>,
    timeout_ms: Arc<AtomicU64>,
    state_sender: mpsc::Sender<Any>,
    req_sender: Option<mpsc::Sender<[bool; 2]>>, // communication between the state_change function and motor thread
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

        // This is a mess
        let chip_address: &str = if Path::new("/sys/class/pwm/pwmchip5").exists() { "pwmchip5" }
                                                                                else { "pwmchip0" };
        for pwm_address in ["0", "1"] {
            if !Path::new(&format!("/sys/class/pwm/{}/pwm{}", chip_address, pwm_address)).exists() {
                let export_loc = format!("/sys/class/pwm/{}/export", chip_address);
                fs::write(export_loc.clone(), pwm_address).map_err(|_e| DecideError::Component { source:
                    StepperMotorError::WriteError { path: export_loc, value: pwm_address.to_string() }.into()
                }).unwrap()
            }
            let configs = vec!["period", "10000", "duty_cycle", "6500", "enable", "1"];
            for pair in configs.chunks(2) {
                let write_loc = format!("/sys/class/pwm/{}/pwm{}/{}",
                                        chip_address, pwm_address, pair[0]);
                fs::write(write_loc.clone(), pair[1]).map_err(|_e| DecideError::Component { source:
                    StepperMotorError::WriteError { path: write_loc, value: pair[1].to_string() }.into()
                }).unwrap()
            }
        }

        StepperMotor {
            running: Arc::new(AtomicBool::new(false)),
            direction: Arc::new(AtomicBool::new(true)),
            timeout_ms: Arc::new(AtomicU64::new(500)),
            state_sender,
            req_sender: None,
            shutdown: None,
        }
    }

    async fn init(&mut self, config: Self::Config) {
        let (req_snd, mut req_rcv) = mpsc::channel(20);
        self.req_sender = Some(req_snd);
        let mut chip1 = Chip::new(config.chip1.clone())
            .map_err(|_e| DecideError::Component { source:
                StepperMotorError::GpioChipError {dev: config.chip1.clone()}.into()
            }).unwrap();
        let mut chip3 = Chip::new(config.chip3.clone())
            .map_err(|_e| DecideError::Component { source:
                StepperMotorError::GpioChipError {dev: config.chip3.clone()}.into()
            }).unwrap();
        let motor_1_handle = StepperMotor::request_lines(&mut chip1, &config.motor1_offsets);
        let motor_3_handle = StepperMotor::request_lines(&mut chip3, &config.motor3_offsets);
        let mut switch_14 = StepperMotor::request_asynclines(&mut chip1, config.switch_offsets[0]);
        let mut switch_15 = StepperMotor::request_asynclines(&mut chip1, config.switch_offsets[1]);

        let running = self.running.clone();
        let direction = self.direction.clone();
        let state_sender = self.state_sender.clone();
        let timeout_ms = Arc::clone(&self.timeout_ms);
        let (shutdown_tx, mut shutdown_rx) = mpsc::channel(1);
        let dt = config.dt;

        let motor_handle = tokio::spawn(async move {
            let mut step = 0;
            loop {
                if shutdown_rx.try_recv().unwrap_err() == mpsc::error::TryRecvError::Disconnected {
                    StepperMotor::pause_motor(&motor_1_handle, &motor_3_handle);
                    break}

                // One of three things can trigger motor running: either the 2 switches, or client signal
                let mut state = StepperMotor::poll_change(&mut switch_14,
                                                      &mut switch_15,
                                                      &mut req_rcv).await;

                if state.running {
                    tracing::debug!("stepper motor running!");
                    running.store(state.running, Ordering::Release);
                    direction.store(state.direction, Ordering::Release);
                    Self::send_state(&state, &state_sender).await;
                    let timer = Instant::now();
                    while Instant::now().duration_since(timer) <
                        Duration::from_millis(timeout_ms.load(Ordering::Acquire)) {
                        step = StepperMotor::run_motor(step, &motor_1_handle,
                                                       &motor_3_handle, state.direction);
                        tokio::time::sleep(Duration::from_micros(dt)).await;
                    }
                    StepperMotor::pause_motor(&motor_1_handle, &motor_3_handle);
                    tracing::debug!("stepper motor stopped!");
                    state.running = false;
                    running.store(state.running, Ordering::Release);
                    direction.store(state.direction, Ordering::Release);
                    Self::send_state(&state, &state_sender).await;
                } else {
                    tokio::time::sleep(Duration::from_micros(dt)).await;
                }
            }
        });
        self.shutdown = Some((motor_handle, shutdown_tx));
        tracing::info!("stepper motor initiated.");
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
                    .map_err(|_e| DecideError::Component { source: StepperMotorError::SendError.into() })
                    .unwrap();
            });
        }
        Ok(())
    }

    fn set_parameters(&mut self, params: Self::Params) -> decide_protocol::Result<()> {
        self.timeout_ms.store(params.timeout_ms, Ordering::Release);
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
            timeout_ms: self.timeout_ms.load(Ordering::Acquire)
        }
    }

    async fn send_state(state: &Self::State, sender: &mpsc::Sender<Any>) {
        tracing::debug!("Emiting state change");
        sender.send(Any {
            type_url: String::from(Self::STATE_TYPE_URL),
            value: state.encode_to_vec(),
        }).await.map_err(|_e| DecideError::Component { source:
            StepperMotorError::SendError.into() }).unwrap();
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
        chip.get_lines(lines)
            .map_err(|_e| DecideError::Component { source:
                StepperMotorError::GpioLineReqError { line: Vec::from(lines) }.into()
            }).unwrap()
            .request(LineRequestFlags::OUTPUT, &[0, 0], "decide-rs")
            .map_err(|_e| DecideError::Component { source:
                StepperMotorError::GpioFlagReqError {line: Vec::from(lines),
                                                     flag: "OUTPUT".to_string()}.into()
            }).unwrap()
    }

    fn request_asynclines(chip: &mut Chip, lines: u32) -> AsyncLineEventHandle {
        let line = chip.get_line(lines)
            .map_err(|_e| DecideError::Component { source:
                StepperMotorError::GpioLineReqError {line: vec![lines]}.into()
            }).unwrap();
        AsyncLineEventHandle::new(
            line.events(
                LineRequestFlags::INPUT,
                EventRequestFlags::BOTH_EDGES,
                "decide-rs"
            ).map_err(|_e| DecideError::Component { source:
                StepperMotorError::GpioFlagReqError {line: vec![lines],
                                                     flag: "INPUT".to_string()}.into()
            }).unwrap()
        ).map_err(|_e| DecideError::Component { source:
            StepperMotorError::GpioAsyncLineError { line: vec![lines as u8] }.into()
        }).unwrap()

    }

    async fn poll_change(sw14: &mut AsyncLineEventHandle,
                         sw15: &mut AsyncLineEventHandle,
                         state_rx: &mut mpsc::Receiver<[bool; 2]>) -> proto::SmState {
        let mut state = proto::SmState {running: false, direction: false};
        tokio::select! {

            Some(event) = sw14.next() => {
                let evt_type = event.map_err(|_e| DecideError::Component { source:
                    StepperMotorError::GpioAsyncEventError.into() }).unwrap().event_type();
                match evt_type {
                    EventType::RisingEdge => {
                        tracing::info!("stepper motor switch 14 pressed");
                        //state.running = false;
                        //state.direction = false;
                    }
                    EventType::FallingEdge => {
                        tracing::debug!("stepper motor switch 14 depressed");
                        state.running = true;
                        //state.direction = false;

                    }
                }
            }
            Some(event) = sw15.next() => {
                let evt_type = event.map_err(|_e| DecideError::Component { source:
                    StepperMotorError::GpioAsyncEventError.into() }).unwrap().event_type();
                match evt_type {
                    EventType::RisingEdge => {
                        tracing::info!("stepper motor switch 15 pressed");
                        //state.running = false;
                        state.direction = true;
                    }
                    EventType::FallingEdge => {
                        tracing::debug!("stepper motor switch 15 depressed");
                        state.running = true;
                        state.direction = true;

                    }
                }
            }
            Some(event) = state_rx.recv() => {
                if event.len() != 2 {
                    tracing::error!("stepper motor event poll received incorrect message");
                } else {
                    state.running = event[0];
                    state.direction = event[1];
                }
            }
        }
        state
    }

    fn run_motor(mut step: usize, handle1: &MultiLineHandle, handle3: &MultiLineHandle, direction: bool) -> usize{
        if direction {
            step = (step + 1) % Self::NUM_HALF_STEPS;
            let step_1_values = &Self::HALF_STEPS[step].0;
            let step_3_values = &Self::HALF_STEPS[step].1;
            handle1.set_values(&step_1_values.0)
                .map_err(|_e| DecideError::Component { source:
                    StepperMotorError::GpioLineSetError { value: Vec::from(step_1_values.0) }.into()
                }).unwrap();
            handle3.set_values(&step_3_values.0)
                .map_err(|_e| DecideError::Component { source:
                StepperMotorError::GpioLineSetError { value: Vec::from(step_3_values.0) }.into()
                }).unwrap();
        } else {
            step = (step - 1) % Self::NUM_HALF_STEPS;
            let step_1_values = &Self::HALF_STEPS[step].0;
            let step_3_values = &Self::HALF_STEPS[step].1;
            handle1.set_values(&step_1_values.0)
                .map_err(|_e| DecideError::Component { source:
                    StepperMotorError::GpioLineSetError { value: Vec::from(step_1_values.0) }.into()
                }).unwrap();
            handle3.set_values(&step_3_values.0)
                .map_err(|_e| DecideError::Component { source:
                    StepperMotorError::GpioLineSetError { value: Vec::from(step_3_values.0) }.into()
                }).unwrap();
        }
        step
    }

    fn pause_motor(handle1: &MultiLineHandle, handle3: &MultiLineHandle) {
        let step_1_values = &Self::ALL_OFF;
        let step_3_values = &Self::ALL_OFF;
        handle1.set_values(&step_1_values.0)
            .map_err(|_e| DecideError::Component { source:
                StepperMotorError::GpioLineSetError { value: Vec::from(step_1_values.0) }.into()
            }).unwrap();
        handle3.set_values(&step_3_values.0)
            .map_err(|_e| DecideError::Component { source:
                StepperMotorError::GpioLineSetError { value: Vec::from(step_3_values.0) }.into()
            }).unwrap();
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

#[derive(Error, Debug)]
pub enum StepperMotorError {
    #[error("could not write value {value:?} to file {path:?}")]
    WriteError{path: String, value: String},
    #[error("could not initialize gpio device {dev:?}")]
    GpioChipError{dev:String},
    #[error("could not request lines {line:?} from gpio device")]
    GpioLineReqError{line: Vec<u32>},
    #[error("could not set gpio lines {line:?} to mode {flag:?}")]
    GpioFlagReqError{line: Vec<u32>, flag: String},
    #[error("could not set gpio line to values {value:?}")]
    GpioLineSetError{value: Vec<u8>},
    #[error("could not get async handle for gpio line {line:?}")]
    GpioAsyncLineError{line: Vec<u8>},
    #[error("could not get async event")]
    GpioAsyncEventError,
    #[error("could not send state update")]
    SendError,
}