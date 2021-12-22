use async_trait::async_trait;
use error::{ClientError, ControllerError, DecideError};
use prost::Message as ProstMessage;
use prost_types::Any;
use serde::{de::DeserializeOwned, Deserialize};
use serde_value::{DeserializerError, Value, ValueDeserializer};
use tokio::sync::mpsc;

pub const DECIDE_VERSION: [u8; 3] = [0xDC, 0xDC, 0x01];

pub const REQ_ENDPOINT: &str = "tcp://127.0.0.1:7897";
pub const PUB_ENDPOINT: &str = "tcp://127.0.0.1:7898";

pub mod proto {
    include!(concat!(env!("OUT_DIR"), "/decide.rs"));
}

mod messages;
pub use messages::{ComponentRequest, GeneralRequest, Request, RequestType};

pub type Result<T> = core::result::Result<T, DecideError>;

#[derive(Debug, Deserialize, Hash, PartialEq, Eq, Clone)]
pub struct ComponentName(pub String);

impl From<&str> for ComponentName {
    fn from(name: &str) -> Self {
        ComponentName(name.into())
    }
}

#[async_trait]
pub trait Component {
    type State: ProstMessage + Default;
    type Params: ProstMessage + Default;
    type Config: DeserializeOwned + Send;
    const STATE_TYPE_URL: &'static str;
    const PARAMS_TYPE_URL: &'static str;

    fn new(config: Self::Config) -> Self;
    async fn init(&mut self, config: Self::Config, state_sender: mpsc::Sender<Any>);
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
