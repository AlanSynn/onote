//! System wall-clock adapter for the [`Clock`](crate::ports::Clock) port.

use chrono::Utc;

use crate::ports::Clock;

pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> chrono::DateTime<chrono::Utc> {
        Utc::now()
    }
}
