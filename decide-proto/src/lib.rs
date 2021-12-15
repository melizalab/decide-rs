use async_trait::async_trait;
use error::{ClientError, ControllerError, DecideError};
use num_derive::{FromPrimitive, ToPrimitive};
use num_traits::{FromPrimitive, ToPrimitive};
use prost::Message as ProstMessage;
use prost_types::Any;
use serde::{de::DeserializeOwned, Deserialize};
use serde_value::{DeserializerError, Value, ValueDeserializer};
use std::convert::TryFrom;
use tmq::Multipart;
use tokio::sync::mpsc;

pub const DECIDE_VERSION: [u8; 3] = [0xDC, 0xDC, 0x01];

pub const REQ_ENDPOINT: &str = "tcp://127.0.0.1:7897";
pub const PUB_ENDPOINT: &str = "tcp://127.0.0.1:7898";

pub mod proto {
    include!(concat!(env!("OUT_DIR"), "/decide.rs"));
}

pub type Result<T> = core::result::Result<T, DecideError>;

#[derive(Debug, Deserialize, Hash, PartialEq, Eq, Clone)]
pub struct ComponentName(pub String);

impl From<&str> for ComponentName {
    fn from(name: &str) -> Self {
        ComponentName(name.into())
    }
}

#[derive(Debug, PartialEq, Clone)]
pub struct Request {
    pub request_type: RequestType,
    pub component: Option<ComponentName>,
    pub body: Vec<u8>,
}

use RequestType::*;
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RequestType {
    Component(ComponentRequest),
    General(GeneralRequest),
}

impl ToPrimitive for RequestType {
    fn to_u64(&self) -> Option<u64> {
        match self {
            Component(c) => c.to_u64(),
            General(g) => g.to_u64(),
        }
    }

    fn to_i64(&self) -> Option<i64> {
        match self {
            Component(c) => c.to_i64(),
            General(g) => g.to_i64(),
        }
    }
}

impl TryFrom<u8> for RequestType {
    type Error = DecideError;

    fn try_from(n: u8) -> core::result::Result<Self, Self::Error> {
        ComponentRequest::from_u8(n)
            .map(Component)
            .or(GeneralRequest::from_u8(n).map(General))
            .ok_or_else(|| ClientError::InvalidRequestType(n).into())
    }
}

#[derive(Debug, Clone, Copy, ToPrimitive, FromPrimitive, PartialEq)]
pub enum ComponentRequest {
    ChangeState = 0x00,
    ResetState = 0x01,
    SetParameters = 0x02,
    GetParameters = 0x12,
}

#[derive(Debug, Clone, Copy, ToPrimitive, FromPrimitive, PartialEq)]
pub enum GeneralRequest {
    RequestLock = 0x20,
    ReleaseLock = 0x21,
    Shutdown = 0x22,
}

#[async_trait]
pub trait Component {
    type State: ProstMessage + Default;
    type Params: ProstMessage + Default;
    type Config: DeserializeOwned + Send;
    const STATE_TYPE_URL: &'static str;
    const PARAMS_TYPE_URL: &'static str;

    fn new(config: Self::Config) -> Self;
    async fn init(&self, config: Self::Config, state_sender: mpsc::Sender<Any>);
    fn change_state(&mut self, state: Self::State) -> Result<()>;
    fn set_parameters(&mut self, params: Self::Params) -> Result<()>;
    fn get_state(&self) -> Self::State;
    fn get_parameters(&self) -> Self::Params;
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

impl From<proto::reply::Result> for proto::Reply {
    fn from(result: proto::reply::Result) -> Self {
        proto::Reply {
            result: Some(result),
        }
    }
}

impl From<Result<proto::reply::Result>> for proto::Reply {
    fn from(result: Result<proto::reply::Result>) -> Self {
        proto::Reply {
            result: Some(match result {
                Err(e) => proto::reply::Result::Error(e.to_string()),
                Ok(r) => r,
            }),
        }
    }
}

impl From<Result<proto::Reply>> for proto::Reply {
    fn from(result: Result<proto::Reply>) -> Self {
        match result {
            Err(e) => proto::Reply {
                result: Some(proto::reply::Result::Error(e.to_string())),
            },
            Ok(r) => r,
        }
    }
}

impl From<Request> for Multipart {
    fn from(request: Request) -> Self {
        match request.component {
            None => Multipart::from(vec![
                &DECIDE_VERSION[..],
                &[request.request_type.to_u8().unwrap()],
                &request.body,
            ]),
            Some(component) => Multipart::from(vec![
                &DECIDE_VERSION[..],
                &[request.request_type.to_u8().unwrap()],
                &request.body,
                component.0.as_bytes(),
            ]),
        }
    }
}

impl TryFrom<Multipart> for Request {
    type Error = DecideError;

    fn try_from(mut zmq_message: Multipart) -> core::result::Result<Self, Self::Error> {
        if zmq_message.len() < 3 {
            return Err(ClientError::BadMultipartLen(zmq_message.len()).into());
        }
        let version = zmq_message.pop_front().unwrap().to_vec();
        if version.as_slice() != DECIDE_VERSION {
            return Err(ClientError::IncompatibleVersion(version).into());
        }
        let request_type = (*zmq_message.pop_front().unwrap())[0];
        let request_type = RequestType::try_from(request_type)?;
        let body = zmq_message.pop_front().unwrap().to_vec();
        let component = match request_type {
            General(_) => None,
            Component(_) => Some(
                zmq_message
                    .pop_front()
                    .ok_or_else(|| ClientError::BadMultipartLen(zmq_message.len()))?
                    .as_str().ok_or(ClientError::InvalidComponent)?
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

impl From<proto::Reply> for Multipart {
    fn from(reply: proto::Reply) -> Self {
        vec![&DECIDE_VERSION[..], &reply.encode_to_vec()].into()
    }
}

impl From<Multipart> for proto::Reply {
    fn from(mut multipart: Multipart) -> Self {
        let _version = multipart.pop_front().unwrap();
        let payload = multipart.pop_front().unwrap();
        proto::Reply::decode(&*payload).unwrap()
    }
}

impl From<proto::Pub> for Multipart {
    fn from(pub_message: proto::Pub) -> Self {
        vec![&DECIDE_VERSION[..], &pub_message.encode_to_vec()].into()
    }
}

pub mod error {
    use super::ComponentName;
    use prost::DecodeError;
    use serde_value::DeserializerError;
    use serde_yaml::Error as YamlError;
    use thiserror::Error;
    use tokio::sync::oneshot;

    #[derive(Error, Debug)]
    pub enum DecideError {
        #[error(transparent)]
        Client {
            #[from]
            source: ClientError,
        },
        #[error("error in component")]
        Component {
            #[from]
            source: anyhow::Error,
        },
        #[error(transparent)]
        Controller {
            #[from]
            source: ControllerError,
        },
    }

    #[derive(Error, Debug)]
    pub enum ClientError {
        #[error("the provided state is invalid for this component")]
        InvalidState,
        #[error("the provided parameters are invalid for this component")]
        InvalidParams,
        #[error("could not decode version string with UTF8")]
        InvalidVersion,
        #[error("could not decode component string with UTF8")]
        InvalidComponent,
        #[error("unrecognized request type: {0}")]
        InvalidRequestType(u8),
        #[error("could not decode message")]
        MessageDecodingError(#[from] DecodeError),
        #[error("unrecognized component identifier `{0:?}`")]
        UnknownComponent(ComponentName),
        #[error("controller is already locked")]
        AlreadyLocked,
        #[error("the provided config identifier `{client}` does not match the config of the controller `{controller}`")]
        ConfigIdMismatch { client: String, controller: String },
        #[error("no state message provided")]
        NoState,
        #[error("no parameters message provided")]
        NoParameters,
        #[error("wrong number of zmq frames: {0}")]
        BadMultipartLen(usize),
        #[error("this controller does not support protcol version `{0:?}`")]
        IncompatibleVersion(Vec<u8>),
        #[error("`Any` protobuf type mismatch: found {actual}, expected {expected}")]
        WrongAnyProtoType { actual: String, expected: String },
    }

    #[derive(Error, Debug)]
    pub enum ControllerError {
        #[error("could not determine config directory")]
        NoConfigDir,
        #[error("could not read from config file `{path:?}`")]
        ConfigReadError {
            path: Option<std::path::PathBuf>,
            source: std::io::Error,
        },
        #[error("could not parse yaml")]
        YamlParseError(#[from] YamlError),
        #[error("component is unable to provide a reply")]
        OneshotRecvDropped(#[from] oneshot::error::RecvError),
        #[error("could not deserialize config")]
        ConfigDeserializationError { source: DeserializerError },
        #[error("unrecognized component driver name `{0}`")]
        UnknownDriver(String),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_to_from_multipart_component() {
        let req = Request {
            request_type: Component(ComponentRequest::ChangeState),
            component: Some(ComponentName("test".into())),
            body: vec![],
        };
        let multipart = Multipart::from(req.clone());
        assert_eq!(req, Request::try_from(multipart).unwrap());
    }

    #[test]
    fn request_to_from_multipart() {
        let req = Request {
            request_type: General(GeneralRequest::RequestLock),
            component: None,
            body: vec![],
        };
        let multipart = Multipart::from(req.clone());
        assert_eq!(req, Request::try_from(multipart).unwrap());
    }
}
