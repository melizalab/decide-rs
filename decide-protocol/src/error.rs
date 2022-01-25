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
