use gpio_cdev::{Chip, AsyncLineEventHandle,
                LineRequestFlags,
                LineHandle, MultiLineHandle,
                EventRequestFlags, EventType,
                errors::Error as GpioError};
use futures::stream::StreamExt;
use tokio::*;
use thiserror;

