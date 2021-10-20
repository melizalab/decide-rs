use decide_proto::{decide, DecideError, DecideRequest, RequestType, DECIDE_VERSION};
use prost::Message;
use std::collections::HashMap;
use directories::ProjectDirs;
use std::fs::File;
use serde::Deserialize;
use std::convert::TryFrom;
use serde_yaml;

mod components;
use components::ComponentKind;

pub struct Components {
    components: HashMap<String, ComponentKind>,
    locked: bool,
}

#[derive(Deserialize)]
struct ComponentsConfig {
    components: Vec<ComponentsConfigItem>,
}

#[derive(Deserialize)]
struct ComponentsConfigItem {
    driver: String,
    config: serde_yaml::Value,
}

impl Components {
    pub fn new() -> Result<Self, DecideError> {
        let config_file = ProjectDirs::from("org", "meliza", "decide").ok_or_else(|| DecideError::NoConfigDir)?.config_dir().join("components.yml");
        let components_config: ComponentsConfig = serde_yaml::from_reader(File::open(config_file).unwrap())?;
        let components = components_config.components.into_iter()
            .map(|item| {
                let component = ComponentKind::try_from(&item.driver[..]).unwrap();
                (item.driver, component)
            }).collect();
        Ok(Components {
            components,
            locked: false,
        })
    }

    pub fn dispatch(&mut self, request: &DecideRequest) -> Result<decide::Reply, DecideError> {
        assert_eq!(request.version.as_slice(), DECIDE_VERSION);
        Ok(match request
            .request_type
            .ok_or_else(|| DecideError::InvalidRequestType)?
        {
            RequestType::ChangeState => {
                self.change_state(decide::StateChange::decode(&*request.body)?)
            }
            RequestType::ResetState => self.reset_state(decide::Component::decode(&*request.body)?),
            RequestType::SetParameters => {
                self.set_parameters(decide::ComponentParams::decode(&*request.body)?)
            }
            RequestType::GetParameters => {
                self.get_parameters(decide::Component::decode(&*request.body)?)
            }
            RequestType::RequestLock => self.request_lock(decide::Config::decode(&*request.body)?),
            RequestType::ReleaseLock => self.release_lock(),
        }
        .into())
    }

    fn change_state(
        &mut self,
        state_change: decide::StateChange,
    ) -> Result<decide::reply::Result, DecideError> {
        let component_name = state_change.component.to_string();
        let component = self
            .components
            .get_mut(&component_name)
            .ok_or_else(|| DecideError::UnknownComponent)?;
        component
            .decode_and_change_state(state_change.state.ok_or_else(|| DecideError::NoState)?)?;
        Ok(decide::reply::Result::Ok(()))
    }

    fn reset_state(
        &mut self,
        component: decide::Component,
    ) -> Result<decide::reply::Result, DecideError> {
        let component_name = component.name.to_string();
        let component = self
            .components
            .get_mut(&component_name)
            .ok_or_else(|| DecideError::UnknownComponent)?;
        component.reset_state()?;
        Ok(decide::reply::Result::Ok(()))
    }

    fn set_parameters(
        &mut self,
        params: decide::ComponentParams,
    ) -> Result<decide::reply::Result, DecideError> {
        let component_name = params.component.to_string();
        let component = self
            .components
            .get_mut(&component_name)
            .ok_or_else(|| DecideError::UnknownComponent)?;
        component.decode_and_set_parameters(
            params.parameters.ok_or_else(|| DecideError::NoParameters)?,
        )?;
        Ok(decide::reply::Result::Ok(()))
    }

    fn get_parameters(
        &mut self,
        component: decide::Component,
    ) -> Result<decide::reply::Result, DecideError> {
        let component_name = component.name.to_string();
        let component = self
            .components
            .get_mut(&component_name)
            .ok_or_else(|| DecideError::UnknownComponent)?;
        let params = component.get_encoded_parameters();
        Ok(decide::reply::Result::Params(params))
    }

    fn request_lock(
        &mut self,
        config: decide::Config,
    ) -> Result<decide::reply::Result, DecideError> {
        if self.locked {
            Err(DecideError::AlreadyLocked)
        } else {
            self.locked = true;
            Ok(decide::reply::Result::Ok(()))
        }
    }

    fn release_lock(&mut self) -> Result<decide::reply::Result, DecideError> {
        self.locked = false;
        Ok(decide::reply::Result::Ok(()))
    }
}
