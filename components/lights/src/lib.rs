use decide_proto::{Component, DecideError};

pub mod lights {
    include!(concat!(env!("OUT_DIR"), "/_.rs"));
}

pub struct Lights {
    state: bool,
    params: lights::Params,
}

impl Component for Lights {
    type State = lights::State;
    type Params = lights::Params;
    type Config = ();
    const STATE_TYPE_URL: &'static str = "melizalab.org/proto/lights_state";
    const PARAMS_TYPE_URL: &'static str = "melizalab.org/proto/lights_params";

    fn new() -> Self {
        Lights {
            state: true,
            params: lights::Params::default(),
        }
    }

    fn init(config: Self::Config) {}

    fn change_state(&mut self, state: Self::State) -> Result<(), DecideError> {
        self.state = state.on;
        Ok(())
    }

    fn reset_state(&mut self) -> Result<(), DecideError> {
        self.state = false;
        Ok(())
    }

    fn set_parameters(&mut self, params: Self::Params) -> Result<(), DecideError> {
        self.params = params;
        Ok(())
    }

    fn get_parameters(&self) -> Self::Params {
        self.params.clone()
    }
}
