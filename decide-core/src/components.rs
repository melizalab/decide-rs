use decide_proto::{Component, error::{DecideError, ControllerError}, Result};
use lights::Lights;
use prost_types::Any;
use serde_value::Value;
use std::convert::TryFrom;
use tokio::sync::mpsc;

macro_rules! impl_component {
    ($($component:ident),*) => {
        pub enum ComponentKind {
            $(
                $component($component),
            )*
        }
        impl ComponentKind {
                pub fn decode_and_change_state(&mut self, message: Any) -> Result<()> {
                    match self {
                        $(
                            ComponentKind::$component(t) => t.decode_and_change_state(message)
                        )*
                    }
                }
                pub fn decode_and_set_parameters(&mut self, message: Any) -> Result<()> {
                    match self {
                        $(
                            ComponentKind::$component(t) => t.decode_and_set_parameters(message)
                        )*
                    }
                }
                pub fn reset_state(&mut self) -> Result<()> {
                    match self {
                        $(
                            ComponentKind::$component(t) => t.reset_state()
                        )*
                    }
                }
                pub fn get_encoded_parameters(&self) -> Any {
                    match self {
                        $(
                            ComponentKind::$component(t) => t.get_encoded_parameters()
                        )*
                    }
                }
                pub async fn init(&self, sender: mpsc::Sender<Any>) {
                    match self {
                        $(
                            ComponentKind::$component(t) => t.init(sender).await,
                        )*
                    }
                }
        }

        impl TryFrom<(&str, Value)> for ComponentKind {
            type Error = DecideError;
            fn try_from((driver_name, config): (&str, Value)) -> Result<Self> {
                match driver_name {
                    $(
                        stringify!($component) => Ok(ComponentKind::$component($component::new($component::deserialize_config(config)?))),
                    )*
                    _ => Err(ControllerError::UnknownDriver(driver_name.into()).into()),
                }
            }
        }
    }
}

impl_component!(Lights);
impl_component!(HouseLight);
impl_component!(StepperMotor);
impl_component!(PeckLeds);
impl_component!(PeckKeys);
