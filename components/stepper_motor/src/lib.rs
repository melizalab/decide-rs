//use std::convert::TryInto;
//use std::os::unix::raw::dev_t;
use decide_proto::{Component, //error::DecideError
};
use prost::Message;
use prost_types::Any;

use std::sync::{Arc, atomic::{AtomicBool, //AtomicU8,
                              Ordering}};
use std::thread;
use std::time::Instant;
use serde::Deserialize;

use async_trait::async_trait;
use tokio::{self,
            sync::mpsc::{Sender},
            time::{//sleep,
                   Duration}
};

use gpio_cdev::{Chip, LineRequestFlags, AsyncLineEventHandle, EventRequestFlags, EventType, LineHandle, MultiLineHandle};
use futures::{//pin_mut, Stream,
              StreamExt};
//use log::{info, trace, warn};

pub mod stepper_motor {
    include!(concat!(env!("OUT_DIR"), "/_.rs"));
}

struct LinesVal([u8; 2]);
pub struct StepperMotor {
    switch: Arc<AtomicBool>,
    on: Arc<AtomicBool>,
    direction: Arc<AtomicBool>,
    timeout: Arc<u64>,
}

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
    fn run_motor(mut step: &usize, mut handle1: &MultiLineHandle, mut handle3: &MultiLineHandle, direction: bool) {
        match direction {
            true => {
                &step = (&step + 1) % Self::NUM_HALF_STEPS;
                let step_1_values = &Self::HALF_STEPS[step].0;
                let step_3_values = &Self::HALF_STEPS[step].1;
                handle1.set_values(&step_1_values.0)
                    //.map_err(|e: GpioError| Error::LinesSetError { lines: &motor1_offsets })
                    .unwrap();
                handle3.set_values(&step_3_values.0)
                    //.map_err(|e: GpioError| Error::LinesSetError { lines: &motor3_offsets })
                    .unwrap();
            }
            false => {
                &step = (&step - 1) % Self::NUM_HALF_STEPS;
                let step_1_values = &Self::HALF_STEPS[step].0;
                let step_3_values = &Self::HALF_STEPS[step].1;
                handle1.set_values(&step_1_values.0)
                    //.map_err(|e: GpioError| Error::LinesSetError { lines: &motor1_offsets })
                    .unwrap();
                handle3.set_values(&step_3_values.0)
                    //.map_err(|e: GpioError| Error::LinesSetError { lines: &motor3_offsets })
                    .unwrap();
            }
        }
    }
    fn pause_motor(mut handle1: &MultiLineHandle, mut handle3: &MultiLineHandle) {
        let step_1_values = &Self::ALL_OFF;
        let step_3_values = &Self::ALL_OFF;
        handle1.set_values(&step_1_values.0)
            //.map_err(|e: GpioError| Error::LinesSetError { lines: &motor1_offsets })
            .unwrap();
        handle3.set_values(&step_3_values.0)
            //.map_err(|e: GpioError| Error::LinesSetError { lines: &motor3_offsets })
            .unwrap();
    }
}

#[async_trait]
impl Component for StepperMotor {
    type State = stepper_motor::State;
    type Params = stepper_motor::Params;
    type Config = Config;
    const STATE_TYPE_URL: &'static str = "melizalab.org/proto/stepper_motor_state";
    const PARAMS_TYPE_URL: &'static str = "melizalab.org/proto/stepper_motor_params";

    fn new(_config: Self::Config) -> Self {
        StepperMotor {
            switch: Arc::new(AtomicBool::new(true)),
            on: Arc::new(AtomicBool::new(false)),
            direction: Arc::new(AtomicBool::new(false)),
            timeout: Arc::new(Default::default()),
        }
    }

    async fn init(&self, config: Self::Config, state_sender: Sender<Any>) {
        let mut chip1 = Chip::new(config.chip1).unwrap();
            //.map_err( |e:GpioError| Error::ChipError { source: e, chip: ChipNumber::Chip1})?;
        let mut chip3 = Chip::new(config.chip3).unwrap();
            //.map_err( |e:GpioError| Error::ChipError {source: e, chip: ChipNumber::Chip3})?;
        let motor_1_handle = chip1
            .get_lines(&config.motor1_offsets)
            .unwrap()//.map_err(|e: GpioError| Error::LinesGetError { lines: &motor1_offsets })?
            .request(LineRequestFlags::OUTPUT, &[0, 0], "stepper").unwrap();
            //.map_err(|e: GpioError| Error::LinesReqError { lines: &motor1_offsets })?;
        let motor_3_handle = chip3
            .get_lines(&config.motor3_offsets).unwrap()
            //.map_err(|e: GpioError| Error::LinesGetError { lines: &motor3_offsets })?
            .request(LineRequestFlags::OUTPUT, &[0, 0], "stepper").unwrap();
            //.map_err(|e: GpioError| Error::LinesReqError { lines: &motor3_offsets })?;

        let switch = self.switch.clone();
        let on = self.on.clone();
        let direction = self.direction.clone();
        let timeout = Arc::clone(&self.timeout);

        let dt = config.dt;
        let switch_offsets = config.switch_offsets;

        //Thread handles motor running and stopping
        let _motor_thread = thread::spawn(move || { //TODO: switch to tokio::spawn thread macro instead of thread::spawn
            let mut step: usize = 0;
            StepperMotor::pause_motor(&motor_1_handle, &motor_3_handle);
            loop {
                //determine if motor is on due to starboard switches or not
                match switch.load(Odering::Acquire) {
                    true => {
                        match on.load(Ordering::Acquire) {
                            true => {StepperMotor::run_motor(&step, &motor_1_handle, &motor_3_handle, direction.load(Ordering::Acquire))}
                            false => {StepperMotor::pause_motor(&motor_1_handle, &motor_3_handle)}
                        }}
                    false => { //due to signal from client instead TODO: make sure client can change switch to false
                        let timer = Instant::now();
                        while Instant::now().duration_since(timer) < Duration::from_millis(*timeout) {
                            match on.load(Ordering::Acquire) {
                                true => {StepperMotor::run_motor(&step, &motor_1_handle, &motor_3_handle, direction.load(Ordering::Acquire))}
                                false => {StepperMotor::pause_motor(&motor_1_handle, &motor_3_handle)}
                            };
                            thread::sleep(Duration::from_micros(dt));
                        }
                    }
                }
                thread::sleep(Duration::from_micros(dt));
            }
        });

        tokio::spawn(async move {
            //init switch lines
            let line_14 = chip1.get_line(switch_offsets[0]).unwrap();
                //.map_err(|e:GpioError| Error::LineGetError {source:e, line: 14})
            let mut handle_14 = AsyncLineEventHandle::new(line_14.events(
                LineRequestFlags::INPUT,
                EventRequestFlags::BOTH_EDGES,
                "stepper_motor_switch"
            ).unwrap()).unwrap();//.map_err(|e: GpioError| Error::LineReqEvtError {line: 14})
                //.map_err(|e: GpioError| Error::AsyncLineReqError {line: 14})

            let line_15 = chip1.get_line(switch_offsets[1])
                //.map_err(|e:GpioError| Error::LineGetError {source:e, line: 15})
                .unwrap();
            let mut handle_15 = AsyncLineEventHandle::new(line_15.events(
                LineRequestFlags::INPUT,
                EventRequestFlags::BOTH_EDGES,
                "stepper_motor_switch"
            ).unwrap()).unwrap();//.map_err(|e: GpioError| Error::LineReqEvtError {line: 15})
                //.map_err(|e: GpioError| Error::AsyncLineReqError {line: 15})

            loop {
                tokio::select! {
                    event = handle_14.next() => {
                        //trace!("Switch 14 pushed");
                        match event.unwrap().unwrap().event_type() {
                            EventType::RisingEdge => {
                                let state = Self::State {
                                    switch: true,
                                    on: false,
                                    direction: false //shouldn't matter either way, but may cause bugs
                                };
                                let message = Any {
                                    value: state.encode_to_vec(),
                                    type_url: Self::STATE_TYPE_URL.into(),
                                };
                                state_sender.send(message).await.unwrap();
                            }
                            EventType::FallingEdge => {
                                let state = Self::State {
                                    switch: true,
                                    on: true,
                                    direction: false
                                };
                                let message = Any {
                                    value: state.encode_to_vec(),
                                    type_url: Self::STATE_TYPE_URL.into(),
                                };
                                state_sender.send(message).await.unwrap();
                            }
                        }
                    }
                    event = handle_15.next() => {
                        //trace!("Switch 15 pushed");
                        match event.unwrap().unwrap().event_type() {
                            EventType::RisingEdge => {
                                let state = Self::State {
                                    switch: true,
                                    on: false,
                                    direction: false //shouldn't matter either way, but may cause bugs
                                };
                                let message = Any {
                                    value: state.encode_to_vec(),
                                    type_url: Self::STATE_TYPE_URL.into(),
                                };
                                state_sender.send(message).await.unwrap();
                            }
                            EventType::FallingEdge => {
                                let state = Self::State {
                                    switch: true,
                                    on: true,
                                    direction: true
                                };
                                let message = Any {
                                    value: state.encode_to_vec(),
                                    type_url: Self::STATE_TYPE_URL.into(),
                                };
                                state_sender.send(message).await.unwrap();
                            }
                        }
                    }
                }
            }
        });
    }

    fn change_state(&mut self, state: Self::State) -> decide_proto::Result<()> {
        self.switch.store(state.switch, Ordering::Release);
        self.on.store(state.on, Ordering::Release);
        self.direction.store(state.direction, Ordering::Release);
        Ok(())
    }

    fn set_parameters(&mut self, params: Self::Params) -> decide_proto::Result<()> {
        *self.timeout = params.timeout;
        Ok(())
    }

    fn get_state(&self) -> Self::State {
        Self::State {
            switch: self.switch.load(Ordering::Acquire),
            on: self.on.load(Ordering::Acquire),
            direction: self.direction.load(Ordering::Acquire)
        }
    }

    fn get_parameters(&self) -> Self::Params {
        Self::Params{
            timeout: self.timeout.clone()
        }
    }
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