use std::sync::{Arc, atomic::{AtomicBool, //AtomicU8,
                              Ordering}, Mutex};
use std::thread;
use std::time::Instant;

use async_trait::async_trait;
use futures::{//pin_mut, Stream,
              StreamExt};
use futures::executor::block_on;
use gpio_cdev::{AsyncLineEventHandle, Chip,
                Error as GpioError, EventRequestFlags,
                EventType,
                LineRequestFlags,
                MultiLineHandle};
use prost::Message;
use prost_types::Any;
use serde::Deserialize;
use tokio::{self,
            sync::mpsc,
            time::{//sleep,
                   Duration}
};

use decide_protocol::{Component, error::{ComponentError as Error, DecideError}};

//use log::{info, trace, warn};

pub mod proto {
    include!(concat!(env!("OUT_DIR"), "/_.rs"));
}

struct LinesVal([u8; 2]);
pub struct StepperMotor {
    switch: Arc<AtomicBool>,
    on: Arc<AtomicBool>,
    direction: Arc<AtomicBool>,
    timeout: Arc<Mutex<u64>>,
    state_sender: mpsc::Sender<Any>,
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
    fn run_motor(mut step: usize, handle1: &MultiLineHandle, handle3: &MultiLineHandle, direction: bool) -> usize{
        if direction {
            step = (step + 1) % Self::NUM_HALF_STEPS;
            let step_1_values = &Self::HALF_STEPS[step].0;
            let step_3_values = &Self::HALF_STEPS[step].1;
            handle1.set_values(&step_1_values.0)
                .map_err(|e: GpioError| Error::LinesSetError { source: (e)})
                .unwrap();
            handle3.set_values(&step_3_values.0)
                .map_err(|e: GpioError| Error::LinesSetError { source: (e)})
                .unwrap();
        } else {
            step = (step - 1) % Self::NUM_HALF_STEPS;
            let step_1_values = &Self::HALF_STEPS[step].0;
            let step_3_values = &Self::HALF_STEPS[step].1;
            handle1.set_values(&step_1_values.0)
                .map_err(|e: GpioError| Error::LinesSetError { source: (e) })
                .unwrap();
            handle3.set_values(&step_3_values.0)
                .map_err(|e: GpioError| Error::LinesSetError { source: (e) })
                .unwrap();
        }
        step
    }
    fn pause_motor(handle1: &MultiLineHandle, handle3: &MultiLineHandle) {
        let step_1_values = &Self::ALL_OFF;
        let step_3_values = &Self::ALL_OFF;
        handle1.set_values(&step_1_values.0)
            .map_err(|e: GpioError| Error::LinesSetError { source: (e) })
            .unwrap();
        handle3.set_values(&step_3_values.0)
            .map_err(|e: GpioError| Error::LinesSetError { source: (e) })
            .unwrap();
    }
}

#[async_trait]
impl Component for StepperMotor {
    type State = proto::State;
    type Params = proto::Params;
    type Config = Config;
    const STATE_TYPE_URL: &'static str = "melizalab.org/proto/stepper_motor_state";
    const PARAMS_TYPE_URL: &'static str = "melizalab.org/proto/stepper_motor_params";

    fn new(_config: Self::Config, state_sender: mpsc::Sender<Any>) -> Self {
        StepperMotor {
            switch: Arc::new(AtomicBool::new(true)),
            on: Arc::new(AtomicBool::new(false)),
            direction: Arc::new(AtomicBool::new(false)),
            timeout: Arc::new(Mutex::new(0)),
            state_sender,
        }
    }

    async fn init(&mut self, config: Self::Config ) {
        let mut chip1 = Chip::new(config.chip1.clone())
            .map_err( |e:GpioError| Error::ChipError { source: e, chip: config.chip1.clone()}).unwrap();
        let mut chip3 = Chip::new(config.chip3.clone())
            .map_err( |e:GpioError| Error::ChipError {source: e, chip: config.chip3.clone()}).unwrap();
        let motor_1_handle = chip1
            .get_lines(&config.motor1_offsets)
            .map_err(|e: GpioError| Error::LinesGetError { source: e, lines: Vec::from(config.motor1_offsets.clone()) }).unwrap()
            .request(LineRequestFlags::OUTPUT, &[0, 0], "stepper")
            .map_err(|e: GpioError| Error::LinesReqError { source: e, lines: Vec::from(config.motor1_offsets.clone()) }).unwrap();
        let motor_3_handle = chip3
            .get_lines(&config.motor3_offsets)
            .map_err(|e: GpioError| Error::LinesGetError { source: e, lines: Vec::from(config.motor3_offsets.clone()) }).unwrap()
            .request(LineRequestFlags::OUTPUT, &[0, 0], "stepper")
            .map_err(|e: GpioError| Error::LinesReqError { source: e, lines: Vec::from(config.motor3_offsets.clone()) }).unwrap();

        let switch = self.switch.clone();
        let on = self.on.clone();
        let direction = self.direction.clone();
        let timeout = Arc::clone(&self.timeout);

        let dt = config.dt;
        let switch_offsets = config.switch_offsets;

        //Thread handles motor running and stopping
        let motor_thread_sender = self.state_sender.clone();
        let _motor_thread = thread::spawn(move || {
            let mut step: usize = 0;
            StepperMotor::pause_motor(&motor_1_handle, &motor_3_handle);
            loop {
                //determine if motor is on due to starboard switches or not
                if switch.load(Ordering::Acquire) {
                    match on.load(Ordering::Acquire) {
                        true => {step = StepperMotor::run_motor(step, &motor_1_handle, &motor_3_handle,
                                                         direction.load(Ordering::Acquire))}
                        false => {StepperMotor::pause_motor(&motor_1_handle, &motor_3_handle)}
                    }
                } else {
                    let timer = Instant::now();
                    // while the timeout period has not completely elapsed:
                    while Instant::now().duration_since(timer) < Duration::from_millis(*timeout.lock().unwrap()) {
                        match on.load(Ordering::Acquire) {
                            true => {step = StepperMotor::run_motor(step, &motor_1_handle, &motor_3_handle,
                                                             direction.load(Ordering::Acquire))}
                            false => {StepperMotor::pause_motor(&motor_1_handle, &motor_3_handle)}
                        };
                        thread::sleep(Duration::from_micros(dt));
                    }
                    //Reset switch state to await for cape switch press
                    switch.store(true, Ordering::Release);
                    let state = Self::State { //perhaps hard code instead of atomic operation for state
                        switch: switch.load(Ordering::Acquire), //false
                        on: on.load(Ordering::Acquire), //true?
                        direction: direction.load(Ordering::Acquire), //refer to line 151
                    };
                    block_on(motor_thread_sender
                        .send(Any {
                            type_url: String::from(Self::STATE_TYPE_URL),
                            value: state.encode_to_vec(),
                        })
                    ).map_err(|e| DecideError::Component { source: e.into() })
                        .unwrap();
                }
                thread::sleep(Duration::from_micros(dt));
            }
        });

        let switch = self.switch.clone();
        let on = self.on.clone();
        let direction = self.direction.clone();
        let switch_sender = self.state_sender.clone();

        let _switch_thread = tokio::spawn( async move {
            //init switch lines
            let line_14 = chip1.get_line(switch_offsets[0])
                .map_err(|e:GpioError| Error::LineGetError {source:e, line: 14}).unwrap();
            let mut handle_14 = AsyncLineEventHandle::new(
                line_14.events(
                    LineRequestFlags::INPUT,
                    EventRequestFlags::BOTH_EDGES,
                    "stepper_motor_switch"
                ).map_err(|e: GpioError| Error::LineReqEvtError { source: e, line: 14}).unwrap()
            ).map_err(|e: GpioError| Error::AsyncEvntReqError {source: e, line: 14}).unwrap();

            let line_15 = chip1.get_line(switch_offsets[1])
                .map_err(|e:GpioError| Error::LineGetError {source:e, line: 15})
                .unwrap();
            let mut handle_15 = AsyncLineEventHandle::new(
                line_15.events(
                    LineRequestFlags::INPUT,
                    EventRequestFlags::BOTH_EDGES,
                    "stepper_motor_switch"
            ).map_err(|e: GpioError| Error::LineReqEvtError {source:e, line: 15}).unwrap()
            ).map_err(|e: GpioError| Error::AsyncEvntReqError {source:e, line: 15}).unwrap();

            loop {
                tokio::select! {
                    Some(event) = handle_14.next() => {
                        let evt_type = event.map_err(|e:GpioError| Error::EventReqError {source:e, line: 14})
                                            .unwrap().event_type();
                        match evt_type {
                            EventType::RisingEdge => {
                                switch.store(true, Ordering::Release);
                                on.store(false, Ordering::Release);
                            }
                            EventType::FallingEdge => {
                                switch.store(true, Ordering::Release);
                                on.store(true, Ordering::Release);
                                direction.store(false, Ordering::Release);
                            }
                        }
                    }
                    Some(event) = handle_15.next() => {
                        let evt_type = event.map_err(|e:GpioError| Error::EventReqError {source:e, line: 15})
                                            .unwrap().event_type();
                        match evt_type {
                            EventType::RisingEdge => {
                                switch.store(true, Ordering::Release);
                                on.store(false, Ordering::Release);
                            }
                            EventType::FallingEdge => {
                                switch.store(true, Ordering::Release);
                                on.store(true, Ordering::Release);
                                direction.store(true, Ordering::Release);
                            }
                        }
                    }
                }
                let state = Self::State { //perhaps hard code instead of atomic operation for state
                    switch: switch.load(Ordering::Acquire), //false
                    on: on.load(Ordering::Acquire), //true?
                    direction: direction.load(Ordering::Acquire), //refer to line 151
                };
                let message = Any {
                    value: state.encode_to_vec(),
                    type_url: Self::STATE_TYPE_URL.into(),
                };
                switch_sender.send(message).await.unwrap();
            }
        });
    }

    fn change_state(&mut self, state: Self::State) -> decide_protocol::Result<()> {
        self.switch.store(state.switch, Ordering::Release);
        self.on.store(state.on, Ordering::Release);
        self.direction.store(state.direction, Ordering::Release);

        let sender = self.state_sender.clone();
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

    fn set_parameters(&mut self, params: Self::Params) -> decide_protocol::Result<()> {
        *self.timeout.lock().unwrap() = params.timeout;
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
            timeout: *self.timeout.lock().unwrap()
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