//! Configuration settings for an individual message log.

mod flush_policy;

pub use flush_policy::FlushPolicy;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

/// The default maximum entries for log segment.
pub const MAX_INDEX_ENTRIES_DEFAULT: u32 = 100_000;

/// The default log retention period.
pub const RETENTION_PERIOD_DEFAULT: Duration = Duration::from_secs(60 * 60 * 24 * 7);

/// The default Cleaner task interval.
pub const CLEANER_INTERVAL_DEFAULT: Duration = Duration::from_secs(60 * 5);

pub type SharedLogConfig = Arc<LogConfig>;

/// The LogConfig struct groups preferences for the log's behaviour, such as settings related to
/// message retention, frequency of flushes, etc, and is shared across each component of the log.
#[derive(Debug, Clone)]
pub struct LogConfig {
    /// Indicates the maximum amount of entries a segment index will retain before a new segment
    /// is created.
    pub max_index_entries: u32,
    /// The path to the directory containing the segment index/data files.
    pub segments_path: PathBuf,
    /// The retention period for each individual segment. Determines when a segment is stale/expired,
    /// and can be cleaned up by the cleaner task.
    pub retention_period: Duration,
    /// The desired interval to poll the cleaner task to discover stale/expired segments.
    pub cleaner_interval: Duration,
    /// The desired flush policy for the log. The flush policy dictates the frequency of flushing based
    /// on the number of writes, and/or a defined interval.
    pub flush_policy: FlushPolicy,
}

impl LogConfig {
    /// Creates a new LogConfig builder with the provided segments path. All remaining fields are assigned
    /// reasonable defaults until overrided.
    pub fn from_path(path: impl AsRef<Path>) -> Self {
        Self {
            max_index_entries: MAX_INDEX_ENTRIES_DEFAULT,
            segments_path: path.as_ref().to_owned(),
            retention_period: RETENTION_PERIOD_DEFAULT,
            cleaner_interval: CLEANER_INTERVAL_DEFAULT,
            flush_policy: FlushPolicy::default(),
        }
    }

    /// Overrides the default `max_index_entries` field.
    pub fn max_index_entries(mut self, max_entries: u32) -> Self {
        self.max_index_entries = max_entries;
        self
    }

    /// Overrides the default `retention_period` field.
    pub fn retention_period(mut self, period: Duration) -> Self {
        self.retention_period = period;
        self
    }

    /// Overrides the default `cleaner_interval` field.
    pub fn cleaner_interval(mut self, interval: Duration) -> Self {
        self.cleaner_interval = interval;
        self
    }

    /// Overrides the default `flush_policy` field.
    pub fn flush_policy(mut self, policy: FlushPolicy) -> Self {
        self.flush_policy = policy;
        self
    }
}
