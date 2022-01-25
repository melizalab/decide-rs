use lights::Lights;
use house_light::HouseLight;
use peckboard::{PeckKeys, PeckLeds};
use stepper_motor::StepperMotor;
use sound::AudioPlayer;

macro_rules! impl_components {
    ($($component:ident),*) => {
        pub use component_kind::ComponentKind;
        mod component_kind {
            use decide_proto::{error::ControllerError, Component, Result};
            use prost_types::Any;
            use serde_value::Value;
            use std::convert::TryFrom;
            use tokio::sync::mpsc;

            mod types {
                $(
                    #[cfg(not(feature = "dummy-mode"))]
                    pub type $component = super::super::$component;

                    #[cfg(feature = "dummy-mode")]
                    pub type $component = super::dummy::$component;
                )*
            }

            pub enum ComponentKind {
                $(
                    $component(types::$component),
                )*
            }

            #[cfg(feature = "dummy-mode")]
            mod dummy {
                use decide_proto::{Result, Component, error::DecideError};
                use tokio::sync::mpsc;
                use async_trait::async_trait;
                use prost_types::Any;
                use prost::Message;

                $(
                    type RealComponent = super::super::$component;

                    pub struct $component {
                        state: <RealComponent as Component>::State,
                        params: <RealComponent as Component>::Params,
                        _config: <RealComponent as Component>::Config,
                        state_sender: Option<mpsc::Sender<Any>>,
                    }

                    #[async_trait]
                    impl Component for $component {
                        type State = <RealComponent as Component>::State;
                        type Params = <RealComponent as Component>::Params;
                        type Config = <RealComponent as Component>::Config;
                        const STATE_TYPE_URL: &'static str = <RealComponent as Component>::STATE_TYPE_URL;
                        const PARAMS_TYPE_URL: &'static str = <RealComponent as Component>::PARAMS_TYPE_URL;

                        fn new(config: Self::Config) -> Self {
                            let state = Self::State::default();
                            let params = Self::Params::default();
                            $component {
                                state,
                                params,
                                _config: config,
                                state_sender: None,
                            }
                        }

                        async fn init(&mut self, _config: Self::Config, sender: mpsc::Sender<Any>) {
                            self.state_sender = Some(sender);
                        }

                        fn change_state(&mut self, state: Self::State) -> Result<()> {
                            tracing::trace!("changing state");
                            self.state = state.clone();
                            let sender = self.state_sender.as_mut().cloned().unwrap();
                            tokio::spawn(async move {
                                sender.send(Any {
                                    type_url: String::from(Self::STATE_TYPE_URL),
                                    value: state.encode_to_vec()
                                }).await.map_err(|e| DecideError::Component{ source: e.into() }).unwrap();
                                tracing::trace!("state changed");
                            });
                            Ok(())
                        }

                        fn set_parameters(&mut self, params: Self::Params) -> Result<()> {
                            self.params = params;
                            Ok(())
                        }

                        fn get_parameters(&self) -> Self::Params {
                            self.params.clone()
                        }

                        fn get_state(&self) -> Self::State {
                            self.state.clone()
                        }
                    }
                    )*
            }

            impl ComponentKind {
                pub fn decode_and_change_state(&mut self, message: Any) -> Result<()> {
                    match self {
                        $(
                            ComponentKind::$component(t) => t.decode_and_change_state(message),
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
                pub async fn init(&mut self, config: Value, sender: mpsc::Sender<Any>) {
                    match self {
                        $(
                            ComponentKind::$component(t) => t.init(types::$component::deserialize_config(config).unwrap(), sender).await,
                        )*
                    }
                }
            }

            impl TryFrom<(&str, Value)> for ComponentKind {
                type Error = anyhow::Error;
                fn try_from((driver_name, config): (&str, Value)) -> anyhow::Result<Self> {
                    match driver_name {
                        $(
                            stringify!($component) => Ok(ComponentKind::$component(
                                    types::$component::new(types::$component::deserialize_config(config)?)
                                    )),
                        )*
                            _ => Err(ControllerError::UnknownDriver(driver_name.into()).into()),
                    }
                }
            }
        }
    }
}

impl_components!(Lights,HouseLight,StepperMotor,PeckLeds,PeckKeys,AudioPlayer);
