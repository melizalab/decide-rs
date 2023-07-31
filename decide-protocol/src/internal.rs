use super::{
    error::{ClientError, ControllerError},
    Result,
};
use async_trait::async_trait;
use prost::Message as ProstMessage;
use prost_types::Any;
use serde::{de::DeserializeOwned, Deserialize};
use serde_value::{DeserializerError, Value, ValueDeserializer};
use tokio::sync::mpsc;

#[async_trait]
pub trait Component {
    type State: ProstMessage + Default;
    type Params: ProstMessage + Default;
    type Config: DeserializeOwned + Send;
    const STATE_TYPE_URL: &'static str;
    const PARAMS_TYPE_URL: &'static str;

    fn new(config: Self::Config, state_sender: mpsc::Sender<Any>) -> Self;
    async fn init(&mut self, config: Self::Config);
    fn change_state(&mut self, state: Self::State) -> Result<()>;
    fn set_parameters(&mut self, params: Self::Params) -> Result<()>;
    fn get_state(&self) -> Self::State;
    fn get_parameters(&self) -> Self::Params;
    async fn shutdown(&mut self);
    fn deserialize_config(config: Value) -> Result<Self::Config> {
        let deserializer: ValueDeserializer<DeserializerError> = ValueDeserializer::new(config);
        let config = Self::Config::deserialize(deserializer)
            .map_err(|e| ControllerError::ConfigDeserializationError { source: e })?;
        Ok(config)
    }
    fn get_encoded_parameters(&self) -> Any {
        Any {
            value: self.get_parameters().encode_to_vec(),
            type_url: Self::PARAMS_TYPE_URL.into(),
        }
    }
    fn get_encoded_state(&self) -> Any {
        Any {
            value: self.get_state().encode_to_vec(),
            type_url: Self::STATE_TYPE_URL.into(),
        }
    }
    fn reset_state(&mut self) -> Result<()> {
        self.change_state(Self::State::default())
    }
    fn decode_and_change_state(&mut self, message: Any) -> Result<()> {
        if message.type_url != Self::STATE_TYPE_URL {
            return Err(ClientError::WrongAnyProtoType {
                actual: message.type_url,
                expected: Self::STATE_TYPE_URL.into(),
            }
            .into());
        }
        println!("In decode_n change state, evoking change state");
        self.change_state(
            Self::State::decode(&*message.value).map_err(ClientError::MessageDecodingError)?,
        )
    }
    fn decode_and_set_parameters(&mut self, message: Any) -> Result<()> {
        if message.type_url != Self::PARAMS_TYPE_URL {
            return Err(ClientError::WrongAnyProtoType {
                actual: message.type_url,
                expected: Self::PARAMS_TYPE_URL.into(),
            }
            .into());
        }
        self.set_parameters(
            Self::Params::decode(&*message.value).map_err(ClientError::MessageDecodingError)?,
        )
    }
}
