use std::sync::{Arc, atomic::{AtomicBool, Ordering}, Mutex, Condvar, mpsc as std_mpsc};
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

use decide_protocol::{Component, error::{DecideError}};

//use log::{info, trace, warn};

pub mod proto {
    include!(concat!(env!("OUT_DIR"), "/_.rs"));
}

struct LinesVal([u8; 2]);
pub struct StepperMotor {
    switch: Arc<AtomicBool>,
    on_paired: Arc<(Mutex<bool>, Condvar)>,
    direction: Arc<AtomicBool>,
    timeout: Arc<Mutex<u64>>,
    state_sender: mpsc::Sender<Any>,
    shutdown: Option<(std::thread::JoinHandle<()>,
                      tokio::task::JoinHandle<()>,
                      std_mpsc::Sender<bool>)>
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
            on_paired: Arc::new((Mutex::new(false), Condvar::new())),
            direction: Arc::new(AtomicBool::new(false)),
            timeout: Arc::new(Mutex::new(0)),
            state_sender,
            shutdown: None,
        }
    }

    async fn init(&mut self, config: Self::Config ) {
        let mut chip1 = Chip::new(config.chip1.clone())
            .map_err(|e| DecideError::Component { source: e.into() })
            .unwrap();
        let mut chip3 = Chip::new(config.chip3.clone())
            .map_err(|e| DecideError::Component { source: e.into() })
            .unwrap();
        let motor_1_handle = chip1
            .get_lines(&config.motor1_offsets)
            .map_err(|e| DecideError::Component { source: e.into() }).unwrap()
            .request(LineRequestFlags::OUTPUT, &[0, 0], "stepper")
            .map_err(|e| DecideError::Component { source: e.into() }).unwrap();
        let motor_3_handle = chip3
            .get_lines(&config.motor3_offsets)
            .map_err(|e| DecideError::Component { source: e.into() }).unwrap()
            .request(LineRequestFlags::OUTPUT, &[0, 0], "stepper")
            .map_err(|e| DecideError::Component { source: e.into() }).unwrap();

        let switch = self.switch.clone();
        let on_paired = self.on_paired.clone();
        let direction = self.direction.clone();
        let timeout = Arc::clone(&self.timeout);

        let dt = config.dt;
        let switch_offsets = config.switch_offsets;

        //Thread handles motor running and stopping
        let (sd_tx, sd_rx) = std_mpsc::channel();
        let motor_thread_sender = self.state_sender.clone();
        let motor_handle = thread::spawn(move || {
            let mut step: usize = 0;
            StepperMotor::pause_motor(&motor_1_handle, &motor_3_handle);
            loop {
                //shutdown
                if sd_rx.try_recv().unwrap_err() == std_mpsc::TryRecvError::Disconnected {
                    StepperMotor::pause_motor(&motor_1_handle, &motor_3_handle);
                    break}

                //switch: True & on: True -> Cape switches pressed, loop with dt pauses
                //switch: False & on: True -> Experiment Script signal
                //switch: True/False & on: False -> Resting state

                let (on_lock, on_cvar) = &*on_paired;
                //wait until on signaled as set to True
                let _on_guard = on_cvar.wait_while(on_lock.lock().unwrap(), |running| {!*running })
                    .map_err(|e| DecideError::Component {source: e.into()}).unwrap();

                let cape_pressed = switch.load(Ordering::Acquire);
                if cape_pressed {
                    tracing::debug!("Switch push detected, running motor");
                    'motor_switch: loop {
                        step = StepperMotor::run_motor(step, &motor_1_handle, &motor_3_handle,
                                                       direction.load(Ordering::Acquire));
                        if !on_lock.lock().unwrap() {
                            StepperMotor::pause_motor(&motor_1_handle, &motor_3_handle);
                            break 'motor_switch
                        }
                        thread::sleep(Duration::from_micros(dt));
                    }
                } else {
                    tracing::debug!("Running motor due to sent signal");
                    let timer = Instant::now();
                    while Instant::now().duration_since(timer) < Duration::from_millis(*timeout.lock().unwrap()) {
                        step = StepperMotor::run_motor(step, &motor_1_handle, &motor_3_handle,
                                                       direction.load(Ordering::Acquire));
                        thread::sleep(Duration::from_micros(dt));
                    };
                    tracing::debug!("Stopping motor after timeout");
                    //Reset to rest
                    let running = on_lock.lock().unwrap();
                    *running = false;
                    //leave Switch on false until flipped by cape press

                    //Send signal
                    tracing::debug!("Sending motor timeout signal");
                    let state = Self::State {
                        switch: switch.load(Ordering::Acquire), //false
                        on: on_lock.lock().unwrap(), //true?
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
            }
        });

        let switch = self.switch.clone();
        let on_paired2 = Arc::clone(&self.on_paired);
        let direction = self.direction.clone();
        let switch_sender = self.state_sender.clone();

        let switch_handle = tokio::spawn( async move {
            //init switch lines
            let line_14 = chip1.get_line(switch_offsets[0])
                .map_err(|e| DecideError::Component { source: e.into() })
                .unwrap();
            let mut handle_14: AsyncLineEventHandle = AsyncLineEventHandle::new(
                line_14.events(
                    LineRequestFlags::INPUT,
                    EventRequestFlags::BOTH_EDGES,
                    "stepper_motor_switch"
                ).map_err(|e| DecideError::Component { source: e.into() }).unwrap()
            ).map_err(|e| DecideError::Component { source: e.into() }).unwrap();

            let line_15 = chip1.get_line(switch_offsets[1])
                .map_err(|e| DecideError::Component { source: e.into() })
                .unwrap();
            let mut handle_15 = AsyncLineEventHandle::new(
                line_15.events(
                    LineRequestFlags::INPUT,
                    EventRequestFlags::BOTH_EDGES,
                    "stepper_motor_switch"
                ).map_err(|e| DecideError::Component { source: e.into() }).unwrap()
            ).map_err(|e| DecideError::Component { source: e.into() }).unwrap();

            let (on_lock, on_cvar) = &*on_paired2;

            loop {
                tokio::select! {
                    Some(event) = handle_14.next() => {
                        let evt_type = event.map_err(|e| DecideError::Component { source: e.into() })
                                            .unwrap().event_type();
                        match evt_type {
                            EventType::RisingEdge => {
                                switch.store(true, Ordering::Release);
                                let mut run = on_lock.lock().unwrap();
                                *run = false;
                            }
                            EventType::FallingEdge => {
                                switch.store(true, Ordering::Release);
                                direction.store(false, Ordering::Release);
                                let mut run = on_lock.lock().unwrap();
                                *run = true;
                                on_cvar.notify_one();
                            }
                        }
                    }
                    Some(event) = handle_15.next() => {
                        let evt_type = event.map_err(|e| DecideError::Component { source: e.into() })
                                            .unwrap().event_type();
                        match evt_type {
                            EventType::RisingEdge => {;
                                switch.store(true, Ordering::Release);
                                let mut run = on_lock.lock().unwrap();
                                *run = false;
                            }
                            EventType::FallingEdge => {
                                switch.store(true, Ordering::Release);
                                direction.store(true, Ordering::Release);
                                let mut run = on_lock.lock().unwrap();
                                *run = true;
                                on_cvar.notify_one();
                            }
                        }
                    }
                }
                let state = Self::State { //perhaps hard code instead of atomic operation for state
                    switch: switch.load(Ordering::Acquire), //false
                    on: on_lock.lock().unwrap(), //true?
                    direction: direction.load(Ordering::Acquire), //refer to line 151
                };
                let message = Any {
                    value: state.encode_to_vec(),
                    type_url: Self::STATE_TYPE_URL.into(),
                };
                switch_sender.send(message).await
                    .map_err(|e| DecideError::Component { source: e.into() })
                    .unwrap();
            }
        });

        self.shutdown = Some((motor_handle, switch_handle, sd_tx))
    }

    fn change_state(&mut self, state: Self::State) -> decide_protocol::Result<()> {
        self.switch.store(state.switch, Ordering::Release);
        self.direction.store(state.direction, Ordering::Release);
        let (on_lock, on_cvar) = &*self.on_paired;
        let mut run = on_lock.lock().unwrap();
        if state.on {
            *run = true;
            on_cvar.notify_one();
        } else {
            *run = false;
        }

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
            tracing::trace!("Stepper-motor state changed");
        });
        Ok(())
    }

    fn set_parameters(&mut self, params: Self::Params) -> decide_protocol::Result<()> {
        *self.timeout.lock().unwrap() = params.timeout;
        Ok(())
    }

    fn get_state(&self) -> Self::State {
        let (on_lock, on_cvar) = &*self.on_paired;

        Self::State {
            switch: self.switch.load(Ordering::Acquire),
            on: on_lock.lock().unwrap(),
            direction: self.direction.load(Ordering::Acquire)
        }
    }

    fn get_parameters(&self) -> Self::Params {
        Self::Params{
            timeout: *self.timeout.lock().unwrap()
        }
    }

    async fn shutdown(&mut self) {
        if let Some((motor_handle, switch_handle, sd_tx)) = self.shutdown.take() {
            switch_handle.abort();
            drop(sd_tx);
            switch_handle.await.unwrap_err();
            motor_handle.join().unwrap();
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