use enumn::N;
use prost::{DecodeError, Message as ProstMessage};
use prost_types::Any;
use std::iter;
use thiserror::Error;
use tmq::{Message, Multipart};

pub const DECIDE_VERSION: [u8; 3] = [0xDC, 0xDC, 0x01];

pub mod decide {
    include!(concat!(env!("OUT_DIR"), "/decide.rs"));
}

#[derive(Debug)]
pub struct DecideRequest {
    pub version: Vec<u8>,
    pub request_type: Option<RequestType>,
    pub body: Vec<u8>,
}

impl From<Multipart> for DecideRequest {
    fn from(mut zmq_message: Multipart) -> Self {
        assert_eq!(zmq_message.len(), 3);
        let version = zmq_message.pop_front().unwrap().to_vec();
        let request_type = (*zmq_message.pop_front().unwrap())[0];
        let request_type = RequestType::n(request_type);
        let body = zmq_message.pop_front().unwrap().to_vec();
        DecideRequest {
            version,
            request_type,
            body,
        }
    }
}

impl From<decide::Reply> for Multipart {
    fn from(reply: decide::Reply) -> Self {
        let version = Message::from(&DECIDE_VERSION[..]);
        let body = Message::from(reply.encode_to_vec());
        iter::once(version).chain(iter::once(body)).collect()
    }
}

#[derive(Debug, Clone, Copy, N)]
pub enum RequestType {
    ChangeState = 0x00,
    ResetState = 0x01,
    SetParameters = 0x02,
    GetParameters = 0x12,
    RequestLock = 0x20,
    ReleaseLock = 0x21,
}

#[derive(Error, Debug)]
pub enum DecideError {
    #[error("The provided state is invalid for this component")]
    InvalidState,
    #[error("The provided parameters are invalid for this component")]
    InvalidParams,
    #[error("Could not decode version string with UTF8")]
    InvalidVersion,
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
}

pub trait Component {
    type State: ProstMessage + Default;
    type Params: ProstMessage + Default;
    type Config;
    const STATE_TYPE_URL: &'static str;
    const PARAMS_TYPE_URL: &'static str;

    fn new() -> Self;
    fn init(config: Self::Config);
    fn change_state(&mut self, state: Self::State) -> Result<(), DecideError>;
    fn set_parameters(&mut self, params: Self::Params) -> Result<(), DecideError>;
    fn get_parameters(&self) -> Self::Params;
    fn get_encoded_parameters(&self) -> Any {
        let mut message = Any::default();
        message.value = self.get_parameters().encode_to_vec();
        message.type_url = Self::PARAMS_TYPE_URL.into();
        message
    }
    fn reset_state(&mut self) -> Result<(), DecideError> {
        self.change_state(Self::State::default())
    }
    fn decode_and_change_state(&mut self, message: Any) -> Result<(), DecideError> {
        assert_eq!(message.type_url, Self::STATE_TYPE_URL);
        self.change_state(Self::State::decode(&*message.value)?)
    }
    fn decode_and_set_parameters(&mut self, message: Any) -> Result<(), DecideError> {
        assert_eq!(message.type_url, Self::PARAMS_TYPE_URL);
        self.set_parameters(Self::Params::decode(&*message.value)?)
    }
}

impl From<Result<decide::reply::Result, DecideError>> for decide::Reply {
    fn from(result: Result<decide::reply::Result, DecideError>) -> Self {
        let mut reply = decide::Reply::default();
        reply.result = Some(match result {
            Err(e) => decide::reply::Result::Error(e.to_string()),
            Ok(r) => r,
        });
        reply
    }
}
