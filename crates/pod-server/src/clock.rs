//! The wall clock, injectable so tests can move time forward.

use std::fmt;
use std::sync::Arc;

use pod_protocol::Timestamp;

/// Source of "now" for anything that stores or compares wall-clock time.
/// Session expiry (spec 2.3) is checked against it, so tests can step past
/// a 24-hour or 90-day boundary without waiting.
#[derive(Clone)]
pub struct Clock(Arc<dyn Fn() -> Timestamp + Send + Sync>);

impl Clock {
    /// The real system clock.
    pub fn system() -> Self {
        Self(Arc::new(Timestamp::now))
    }

    /// A clock that calls `now` for every reading.
    pub fn from_fn(now: impl Fn() -> Timestamp + Send + Sync + 'static) -> Self {
        Self(Arc::new(now))
    }

    pub fn now(&self) -> Timestamp {
        (self.0)()
    }
}

impl fmt::Debug for Clock {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Clock")
    }
}
