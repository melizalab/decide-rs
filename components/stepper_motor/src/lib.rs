use decide_proto::{Component, DecideError, DecideGpioError};
use prost::Message;
use prost_types::Any;

use std::sync::{Arc, atomic::{AtomicBool, AtomicU8, Ordering}};
use serde::Deserialize;

use async_trait::async_trait;
use tokio::{self,
            sync::mpsc::{self, Sender},
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

struct DualOffset([u8; 2]);
struct StepperMotorApparatus {
    stepper_motor: StepperMotor,
    switch: Switch,
}
struct StepperMotor {
    on: Arc<AtomicBool>,
    dir: Arc<AtomicBool>
}
struct Switch {
    manual: Arc<AtomicBool>,
    switch_line_14: AsyncLineEventHandle,
    switch_line_15: AsyncLineEventHandle
}

#[async_trait]
impl Component for StepperMotorApparatus {
    type State = stepper_motor::State;
    type Params = stepper_motor::Params;
    type Config = Config;
    const STATE_TYPE_URL: &'static str = "melizalab.org/proto/stepper_motor_state";
    const PARAMS_TYPE_URL: &'static str = "melizalab.org/proto/stepper_motor_params";

    fn new() -> Self {
        StepperMotorApparatus{
            stepper_motor: StepperMotor {},
            switch: Switch {}
        }
    }

    fn init(&self, config: Self::Config, state_sender: Sender<Any>) {
        todo!()
    }

    fn change_state(&mut self, state: Self::State) -> decide_proto::Result<()> {
        todo!()
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

#[derive(Deserialize)]
struct Config {
    chip1: String,
    chip3: String,
    switch_lines: [u32; 2],
    motor1_lines: [u32; 2],
    motor3_lines: [u32; 2],
}