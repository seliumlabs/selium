use rkyv::{Archive, Deserialize, Serialize};

/// Snapshot of the host clock values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Archive, Serialize, Deserialize)]
#[rkyv(bytecheck())]
pub struct TimeNow {
    /// Unix timestamp in milliseconds.
    pub unix_ms: u64,
    /// Monotonic timestamp in milliseconds.
    pub monotonic_ms: u64,
}

/// Request to sleep for a duration in milliseconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Archive, Serialize, Deserialize)]
#[rkyv(bytecheck())]
pub struct TimeSleep {
    /// Duration to sleep in milliseconds.
    pub duration_ms: u64,
}
