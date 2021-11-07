use decide_proto::{Component, DecideError};
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
                pub fn decode_and_change_state(&mut self, message: Any) -> Result<(), DecideError> {
                    match self {
                        $(
                            ComponentKind::$component(t) => t.decode_and_change_state(message)
                        )*
                    }
                }
                pub fn decode_and_set_parameters(&mut self, message: Any) -> Result<(), DecideError> {
                    match self {
                        $(
                            ComponentKind::$component(t) => t.decode_and_set_parameters(message)
                        )*
                    }
                }
                pub fn reset_state(&mut self) -> Result<(), DecideError> {
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
                pub fn deserialize_and_init(&self, config: Value, sender: mpsc::Sender<Any>) -> Result<(), DecideError> {
                    match self {
                        $(
                            ComponentKind::$component(t) => t.deserialize_and_init(config, sender),
                        )*
                    }
                }
        }

        impl TryFrom<&str> for ComponentKind {
            type Error = DecideError;
            fn try_from(driver_name: &str) -> Result<Self, Self::Error> {
                match driver_name {
                    $(
                        stringify!($component) => Ok(ComponentKind::$component($component::new())),
                    )*
                    _ => Err(DecideError::UnknownDriver),
                }
            }
        }
    }
}

impl_component!(Lights);
