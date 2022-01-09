/*!
    `decide-protcol` defines both the external or client API
    as well as the internal or component API.
*/

use error::DecideError;
use serde::Deserialize;

pub mod proto {
    include!(concat!(env!("OUT_DIR"), "/decide.rs"));
}

mod external;
pub use external::{
    ComponentRequest, GeneralRequest, Request, RequestType, PUB_ENDPOINT, REQ_ENDPOINT,
};

mod internal;
pub use internal::Component;

pub type Result<T> = core::result::Result<T, DecideError>;

#[derive(Debug, Deserialize, Hash, PartialEq, Eq, Clone)]
pub struct ComponentName(pub String);

impl From<&str> for ComponentName {
    fn from(name: &str) -> Self {
        ComponentName(name.into())
    }
}

pub mod error;
