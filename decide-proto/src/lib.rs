use async_trait::async_trait;
use num_derive::{FromPrimitive, ToPrimitive};
use num_traits::FromPrimitive;
use prost::{DecodeError, Message as ProstMessage};
use prost_types::Any;
use serde::{de::DeserializeOwned, Deserialize};
use serde_value::{DeserializerError, Value, ValueDeserializer};
use serde_yaml::Error as YamlError;
use std::convert::TryFrom;
use thiserror::Error;
use tmq::Multipart;
use tokio::sync::{mpsc, oneshot};

pub const DECIDE_VERSION: [u8; 3] = [0xDC, 0xDC, 0x01];

pub const REQ_ENDPOINT: &str = "tcp://127.0.0.1:7897";
pub const PUB_ENDPOINT: &str = "tcp://127.0.0.1:7898";

pub mod decide {
    include!(concat!(env!("OUT_DIR"), "/decide.rs"));
}

pub type Result<T> = core::result::Result<T, DecideError>;

#[derive(Debug, Deserialize, Hash, PartialEq, Eq, Clone)]
pub struct ComponentName(String);

impl From<&str> for ComponentName {
    fn from(name: &str) -> Self {
        ComponentName(name.into())
    }
}

#[derive(Debug)]
pub struct Request {
    pub request_type: RequestType,
    pub component: Option<ComponentName>,
    pub body: Vec<u8>,
}

use RequestType::*;
#[derive(Debug, Clone, Copy)]
pub enum RequestType {
    Component(ComponentRequest),
    General(GeneralRequest),
}

impl TryFrom<u8> for RequestType {
    type Error = DecideError;

    fn try_from(n: u8) -> core::result::Result<Self, Self::Error> {
        ComponentRequest::from_u8(n)
            .map(Component)
            .or(GeneralRequest::from_u8(n).map(General))
            .ok_or_else(|| DecideError::InvalidRequestType)
    }
}

#[derive(Debug, Clone, Copy, ToPrimitive, FromPrimitive)]
pub enum ComponentRequest {
    ChangeState = 0x00,
    ResetState = 0x01,
    SetParameters = 0x02,
    GetParameters = 0x12,
}

#[derive(Debug, Clone, Copy, ToPrimitive, FromPrimitive)]
pub enum GeneralRequest {
    RequestLock = 0x20,
    ReleaseLock = 0x21,
}

#[async_trait]
pub trait Component {
    type State: ProstMessage + Default;
    type Params: ProstMessage + Default;
    type Config: DeserializeOwned + Send;
    const STATE_TYPE_URL: &'static str;
    const PARAMS_TYPE_URL: &'static str;

    fn new(config: Self::Config) -> Self;
    async fn init(&self, state_sender: mpsc::Sender<Any>);
    fn change_state(&mut self, state: Self::State) -> Result<()>;
    fn set_parameters(&mut self, params: Self::Params) -> Result<()>;
    fn get_state(&self) -> Self::State;
    fn get_parameters(&self) -> Self::Params;
    fn deserialize_config(config: Value) -> Result<Self::Config> {
        let deserializer: ValueDeserializer<DeserializerError> = ValueDeserializer::new(config);
        let config = Self::Config::deserialize(deserializer)
            .map_err(|_| DecideError::ConfigDeserializationError)?;
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
            return Err(DecideError::WrongAnyProtoType);
        }
        self.change_state(Self::State::decode(&*message.value)?)
    }
    fn decode_and_set_parameters(&mut self, message: Any) -> Result<()> {
        if message.type_url != Self::PARAMS_TYPE_URL {
            return Err(DecideError::WrongAnyProtoType);
        }
        self.set_parameters(Self::Params::decode(&*message.value)?)
    }
}

impl From<decide::reply::Result> for decide::Reply {
    fn from(result: decide::reply::Result) -> Self {
        decide::Reply {
            result: Some(result),
        }
    }
}

impl From<Result<decide::reply::Result>> for decide::Reply {
    fn from(result: Result<decide::reply::Result>) -> Self {
        decide::Reply {
            result: Some(match result {
                Err(e) => decide::reply::Result::Error(e.to_string()),
                Ok(r) => r,
            }),
        }
    }
}

impl From<Result<decide::Reply>> for decide::Reply {
    fn from(result: Result<decide::Reply>) -> Self {
        match result {
            Err(e) => decide::Reply {
                result: Some(decide::reply::Result::Error(e.to_string())),
            },
            Ok(r) => r,
        }
    }
}

impl TryFrom<Multipart> for Request {
    type Error = DecideError;

    fn try_from(mut zmq_message: Multipart) -> core::result::Result<Self, Self::Error> {
        if zmq_message.len() < 3 {
            return Err(DecideError::BadMultipartLen);
        }
        let version = zmq_message.pop_front().unwrap().to_vec();
        if version.as_slice() != DECIDE_VERSION {
            return Err(DecideError::IncompatibleVersion);
        }
        let request_type = (*zmq_message.pop_front().unwrap())[0];
        let request_type = RequestType::try_from(request_type)?;
        let body = zmq_message.pop_front().unwrap().to_vec();
        let component = match request_type {
            General(_) => None,
            Component(_) => Some(
                zmq_message
                    .pop_front()
                    .ok_or_else(|| DecideError::BadMultipartLen)?
                    .as_str()
                    .ok_or_else(|| DecideError::InvalidComponent)?
                    .into(),
            ),
        };
        Ok(Request {
            request_type,
            body,
            component,
        })
    }
}

impl From<decide::Reply> for Multipart {
    fn from(reply: decide::Reply) -> Self {
        vec![&DECIDE_VERSION[..], &reply.encode_to_vec()].into()
    }
}

impl From<decide::Pub> for Multipart {
    fn from(pub_message: decide::Pub) -> Self {
        vec![&DECIDE_VERSION[..], &pub_message.encode_to_vec()].into()
    }
}

#[derive(Error, Debug)]
pub enum DecideError {
    #[error("The provided state is invalid for this component")]
    InvalidState,
    #[error("The provided parameters are invalid for this component")]
    InvalidParams,
    #[error("Could not decode version string with UTF8")]
    InvalidVersion,
    #[error("Could not decode component string with UTF8")]
    InvalidComponent,
    #[error("Unrecognized request type")]
    InvalidRequestType,
    #[error("Could not decode message")]
    MessageDecodingError(#[from] DecodeError),
    #[error("Unrecognized component identifier")]
    UnknownComponent,
    #[error("Unrecognized component driver name")]
    UnknownDriver,
    #[error("Controller is already locked")]
    AlreadyLocked,
    #[error("No state message provided")]
    NoState,
    #[error("No parameters message provided")]
    NoParameters,
    #[error("Could not find config directory")]
    NoConfigDir,
    #[error("Could not open config file")]
    ConfigReadError,
    #[error("Could not parse yaml")]
    YamlParseError(#[from] YamlError),
    #[error("This component was given a protobuf of the wrong type")]
    WrongAnyProtoType,
    #[error("Wrong number of zmq frames")]
    BadMultipartLen,
    #[error("The given message specified a version that this controller does not support")]
    IncompatibleVersion,
    #[error("Component is unable to provide a reply")]
    OneshotRecvDropped(#[from] oneshot::error::RecvError),
    #[error("Could not deserialize config")]
    ConfigDeserializationError,
    #[error("The provided config identifier does not match the config of the controller")]
    ConfigIdMismatch,
}
