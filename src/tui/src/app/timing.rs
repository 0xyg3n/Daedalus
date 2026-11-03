//! Process-local waits use elapsed time; durable evidence keeps UTC timestamps.
use std::time::{Duration, Instant};

/// Store a duration rather than adding it to an Instant: a server may return
/// a delay larger than the platform's representable future deadline.
#[derive(Clone, Copy)]
pub(crate) struct Delay {
    started: Instant,
    duration: Duration,
}
impl Delay {
    pub(crate) fn new(started: Instant, duration: Duration) -> Self {
        Self { started, duration }
    }
    pub(crate) fn ready(self, now: Instant) -> bool {
        now.saturating_duration_since(self.started) >= self.duration
    }
}
