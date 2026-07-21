use std::time::Duration;

use embedded_hal_async::delay::DelayNs;
use tokio::time;

pub struct LinuxDelay;

impl LinuxDelay {
    pub fn new() -> Self {
        // Doesn't do anything useful, but it's here to match this crate's API.
        Self
    }
}

impl DelayNs for LinuxDelay {
    async fn delay_ns(&mut self, ns: u32) { time::sleep(Duration::from_nanos(ns as u64)).await;  }

    async fn delay_us(&mut self, us: u32) {
        time::sleep(Duration::from_micros(us as u64)).await;
    }

    async fn delay_ms(&mut self, ms: u32) {
        time::sleep(Duration::from_millis(ms as u64)).await;
    }
}