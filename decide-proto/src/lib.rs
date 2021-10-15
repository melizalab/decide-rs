use enumn::N;
use prost::{DecodeError, Message as ProtoMessage};
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
}

trait Component {
    type State;
    type Params;

    fn change_state(state: Self::State) -> Result<(), DecideError>;
    fn reset_state() -> Result<(), DecideError>;
    fn set_parameters(params: Self::Params) -> Result<(), DecideError>;
    fn get_parameters() -> Self::Params;
}
