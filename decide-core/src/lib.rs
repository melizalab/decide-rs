use decide_proto::{decide, DecideError, DecideRequest, RequestType, DECIDE_VERSION};
use prost::Message;

pub struct ComponentList {}

impl ComponentList {
    pub fn dispatch(&mut self, request: &DecideRequest) -> Result<decide::Reply, DecideError> {
        assert_eq!(request.version.as_slice(), DECIDE_VERSION);
        Ok(
            match request
                .request_type
                .ok_or_else(|| DecideError::InvalidRequestType)?
            {
                RequestType::ChangeState => {
                    self.change_state(decide::StateChange::decode(&*request.body)?)
                }
                RequestType::ResetState => {
                    self.reset_state(decide::Component::decode(&*request.body)?)
                }
                RequestType::SetParameters => {
                    self.set_parameters(decide::ComponentParams::decode(&*request.body)?)
                }
                RequestType::GetParameters => {
                    self.get_parameters(decide::Component::decode(&*request.body)?)
                }
                RequestType::RequestLock => {
                    self.request_lock(decide::Config::decode(&*request.body)?)
                }
                RequestType::ReleaseLock => self.release_lock(),
            },
        )
    }

    fn change_state(&mut self, new_state: decide::StateChange) -> decide::Reply {
        let mut reply = decide::Reply::default();
        reply.result = Some(decide::reply::Result::Ok(()));
        reply
    }

    fn reset_state(&mut self, component: decide::Component) -> decide::Reply {
        let mut reply = decide::Reply::default();
        reply.result = Some(decide::reply::Result::Ok(()));
        reply
    }

    fn set_parameters(&mut self, params: decide::ComponentParams) -> decide::Reply {
        let mut reply = decide::Reply::default();
        reply.result = Some(decide::reply::Result::Ok(()));
        reply
    }

    fn get_parameters(&self, component: decide::Component) -> decide::Reply {
        let mut reply = decide::Reply::default();
        reply.result = Some(decide::reply::Result::Ok(()));
        reply
    }

    fn request_lock(&mut self, config: decide::Config) -> decide::Reply {
        let mut reply = decide::Reply::default();
        reply.result = Some(decide::reply::Result::Ok(()));
        reply
    }

    fn release_lock(&mut self) -> decide::Reply {
        let mut reply = decide::Reply::default();
        reply.result = Some(decide::reply::Result::Ok(()));
        reply
    }
}
