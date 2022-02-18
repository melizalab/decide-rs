use super::{error::ClientError, proto, ComponentName, DecideError, Result};
use num_derive::{FromPrimitive, ToPrimitive};
use num_traits::{FromPrimitive, ToPrimitive};
use prost::Message as ProstMessage;
use std::convert::TryFrom;
use tmq::Multipart;

pub const DECIDE_VERSION: &[u8] = b"DCDC01";

pub const REQ_ENDPOINT: &str = "tcp://127.0.0.1:7897";
pub const PUB_ENDPOINT: &str = "tcp://127.0.0.1:7898";

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
            .or_else(|| GeneralRequest::from_u8(n).map(General))
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
                DECIDE_VERSION,
                &[request.request_type.to_u8().unwrap()],
                &request.body,
            ]),
            Some(component) => Multipart::from(vec![
                DECIDE_VERSION,
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
                    .as_str()
                    .ok_or(ClientError::InvalidComponent)?
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
        vec![DECIDE_VERSION, &reply.encode_to_vec()].into()
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
        vec![DECIDE_VERSION, &pub_message.encode_to_vec()].into()
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
