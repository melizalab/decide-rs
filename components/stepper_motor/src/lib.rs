use std::os::unix::raw::dev_t;
use decide_proto::{Component, DecideError};
use prost::Message;
use prost_types::Any;

use std::sync::{Arc, atomic::{AtomicBool, AtomicU8, Ordering}};
use std::thread;
use serde::Deserialize;

use async_trait::async_trait;
use tokio::{self,
            sync::mpsc::{self, Sender, Receiver},
            time::{sleep, Duration}
};

use gpio_cdev::{Chip,
                LineRequestFlags,
                AsyncLineEventHandle,
                EventRequestFlags, EventType,
                errors::Error as GpioError};
use thiserror;
use futures::{pin_mut, Stream, StreamExt};
use log::{info, trace, warn};

struct LinesVal([u8; 2]);
struct StepperMotorApparatus {
    stepper_motor: StepperMotor
}
struct StepperMotor {
    on: Arc<AtomicBool>,
    direction: Arc<AtomicBool>
}

impl StepperMotor {
    const NUM_HALF_STEPS: usize = 8;
    const ALL_OFF: LinesVal = LinesVal([0,0]);
    const HALF_STEPS: [(LinesVal, LinesVal); 8] = [
        (LinesVal([0,1]),LinesVal([1,0])),
        (LinesVal([0,1]),LinesVal([0,0])),
        (LinesVal([0,1]),LinesVal([0,1])),
        (LinesVal([0,0]),LinesVal([0,1])),
        (LinesVal([1,0]),LinesVal([0,1])),
        (LinesVal([1,0]),LinesVal([0,0])),
        (LinesVal([1,0]),LinesVal([1,0])),
        (LinesVal([0,0]),LinesVal([1,0]))
    ];

    fn new() -> Self {
        StepperMotor{
            on: Arc::new(AtomicBool::new(false)),
            direction: Arc::new(AtomicBool::new(false))
        }
    }
    fn init(&self, chip1: &mut Chip, chip3: &mut Chip, motor1_offsets: [u32; 2], motor3_offsets: [u32; 2], dt: u64) -> Result<(), E> {
        let motor_1_handle = chip1
            .get_lines(&motor1_offsets.0)
            .unwrap()
            //.map_err(|e: GpioError| Error::LinesGetError { lines: &motor1_offsets })?
            .request(LineRequestFlags::OUTPUT, &[0, 0], "stepper")
            .unwrap();
            //.map_err(|e: GpioError| Error::LinesReqError { lines: &motor1_offsets })?;
        let motor_3_handle = chip3
            .get_lines(&motor3_offsets.0)
            .unwrap()
            //.map_err(|e: GpioError| Error::LinesGetError { lines: &motor3_offsets })?
            .request(LineRequestFlags::OUTPUT, &[0, 0], "stepper")
            .unwrap();
            //.map_err(|e: GpioError| Error::LinesReqError { lines: &motor3_offsets })?;

        let on = &self.on.clone();
        let direction = &self.direction.clone();

        let _motor_thread = thread::spawn(move || { //TODO: switch to tokio::spawn thread macro instead of thread::spawn
            let mut step: usize = 0;
            motor_1_handle.set_values(&Self::ALL_OFF.0)
                //.map_err(|e: GpioError| Error::LinesSetError { lines: &motor1_offsets })
                .unwrap();
            motor_3_handle.set_values(&Self::ALL_OFF.0)
                //.map_err(|e: GpioError| Error::LinesSetError { lines: &motor3_offsets })
                .unwrap();
            loop {
                match on.load(Ordering::Relaxed) {
                    true => {
                        match direction.load(Ordering::Relaxed) {
                            true => {
                                step = (step + 1) % Self::NUM_HALF_STEPS;
                                let step_1_values = &Self::HALF_STEPS[step].0;
                                let step_3_values = &Self::HALF_STEPS[step].1;
                                motor_1_handle.set_values(&step_1_values.0)
                                    //.map_err(|e: GpioError| Error::LinesSetError { lines: &motor1_offsets })
                                    .unwrap();
                                motor_3_handle.set_values(&step_3_values.0)
                                    //.map_err(|e: GpioError| Error::LinesSetError { lines: &motor3_offsets })
                                    .unwrap();
                            }
                            false => {
                                step = (step - 1) % Self::NUM_HALF_STEPS;
                                let step_1_values = &Self::HALF_STEPS[step].0;
                                let step_3_values = &Self::HALF_STEPS[step].1;
                                motor_1_handle.set_values(&step_1_values.0)
                                    //.map_err(|e: GpioError| Error::LinesSetError { lines: &motor1_offsets })
                                    .unwrap();
                                motor_3_handle.set_values(&step_3_values.0)
                                    //.map_err(|e: GpioError| Error::LinesSetError { lines: &motor3_offsets })
                                    .unwrap();
                            }
                        }
                    }
                    false => {
                        let step_1_values = &Self::ALL_OFF;
                        let step_3_values = &Self::ALL_OFF;
                        motor_1_handle.set_values(&step_1_values.0)
                            //.map_err(|e: GpioError| Error::LinesSetError { lines: &motor1_offsets })
                            .unwrap();
                        motor_3_handle.set_values(&step_3_values.0)
                            //.map_err(|e: GpioError| Error::LinesSetError { lines: &motor3_offsets })
                            .unwrap();
                    }
                };
                thread::sleep(Duration::from_micros(dt));
            }
        });
    }
}

#[async_trait]
impl Component for StepperMotorApparatus {
    type State = stepper_motor::State;
    type Params = stepper_motor::Params;
    type Config = Config;
    const STATE_TYPE_URL: &'static str = "melizalab.org/proto/stepper_motor_state";
    const PARAMS_TYPE_URL: &'static str = "melizalab.org/proto/stepper_motor_params";

    fn new(_config: Self::Config) -> Self {
        StepperMotorApparatus{
            stepper_motor: StepperMotor::new()
        }
    }

    fn init(&self, config: Self::Config, state_sender: Sender<Any>) {
        let mut chip1 = Chip::new(config.chip1)
            .unwrap();
            //.map_err( |e:GpioError| Error::ChipError { source: e, chip: ChipNumber::Chip1})?;
        let mut chip3 = Chip::new(chip3)
            .unwrap();
            //.map_err( |e:GpioError| Error::ChipError {source: e, chip: ChipNumber::Chip3})?;
        self.stepper_motor.init(&mut chip1, &mut chip3, config.motor1_offsets, config.motor3_offsets, config.dt).unwrap();
        let on = &self.stepper_motor.on.clone();
        let direction = &self.stepper_motor.direction.clone();

        tokio::spawn(async move {
            //init switch lines
            let line_14 = chip1.get_line(14)
                //.map_err(|e:GpioError| Error::LineGetError {source:e, line: 14})
                .unwrap();
            let handle_14 = AsyncLineEventHandle::new(line_14.events(
                LineRequestFlags::INPUT,
                EventRequestFlags::BOTH_EDGES,
                "stepper_motor_switch"
            )//.map_err(|e: GpioError| Error::LineReqEvtError {line: 14})
                .unwrap())
                //.map_err(|e: GpioError| Error::AsyncLineReqError {line: 14})
                .unwrap();

            let line_15 = chip1.get_line(15)
                //.map_err(|e:GpioError| Error::LineGetError {source:e, line: 15})
                .unwrap();
            let handle_15 = AsyncLineEventHandle::new(line_15.events(
                LineRequestFlags::INPUT,
                EventRequestFlags::BOTH_EDGES,
                "stepper_motor_switch"
            )//.map_err(|e: GpioError| Error::LineReqEvtError {line: 15})
                .unwrap())
                //.map_err(|e: GpioError| Error::AsyncLineReqError {line: 15})
                .unwrap();

            loop {
                tokio::select! {
                    event = handle_14.next() => {
                        trace!("Switch 14 pushed");
                        match event.unwrap().unwrap().event_type() {
                            EventType::RisingEdge => {on.store(false, Ordering::Relaxed)}
                            EventType::FallingEdge => {
                                on.store(true, Ordering::Relaxed);
                                direction.store(false, Ordering::Relaxed);
                            }
                        }
                    }
                    event = handle_15.next() => {
                        trace!("Switch 15 pushed");
                        match event.unwrap().unwrap().event_type() {
                            EventType::RisingEdge => {on.store(false, Ordering::Relaxed)}
                            EventType::FallingEdge => {
                                on.store(true, Ordering::Relaxed);
                                direction.store(true, Ordering::Relaxed);
                            }
                        }
                    }
                }
            }
        });
    }

    fn change_state(&mut self, state: Self::State) -> decide_proto::Result<()> {
        self.stepper_motor.on.store(state.on, Ordering::Relaxed);
        self.stepper_motor.direction.store(state.direction, Ordering::Relaxed);
        Ok(())
    }

    fn set_parameters(&mut self, _params: Self::Params) -> decide_proto::Result<()> {
        Ok(())
    }

    fn get_state(&self) -> Self::State {
        Self::State {
            on: self.stepper_motor.on.load(Ordering::Relaxed),
            direction: self.stepper_motor.direction.load(Ordering::Relaxed)
        };
        Ok(())
    }

    fn get_parameters(&self) -> Self::Params {
        Ok(())
    }
}


#[derive(Deserialize)]
struct Config {
    chip1: String,
    chip3: String,
    switch_offsets: [u32; 2], //14,15
    motor1_offsets: [u32; 2], //13, 12
    motor3_offsets: [u32; 2], //19,21
    dt: u64, //2000
}