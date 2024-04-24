mod flush_policy;

pub use flush_policy::FlushPolicy;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

pub const MAX_INDEX_ENTRIES_DEFAULT: u32 = 100_000;
pub const RETENTION_PERIOD_DEFAULT: Duration = Duration::from_secs(60 * 60 * 24 * 7);
pub const CLEANER_INTERVAL_DEFAULT: Duration = Duration::from_secs(60 * 5);

pub type SharedLogConfig = Arc<LogConfig>;

#[derive(Debug, Clone)]
pub struct LogConfig {
    pub max_index_entries: u32,
    pub segments_path: PathBuf,
    pub retention_period: Duration,
    pub cleaner_interval: Duration,
    pub flush_policy: FlushPolicy,
}

impl LogConfig {
    pub fn from_path(path: impl AsRef<Path>) -> Self {
        Self {
            max_index_entries: MAX_INDEX_ENTRIES_DEFAULT,
            segments_path: path.as_ref().to_owned(),
            retention_period: RETENTION_PERIOD_DEFAULT,
            cleaner_interval: CLEANER_INTERVAL_DEFAULT,
            flush_policy: FlushPolicy::default(),
        }
    }

    pub fn max_index_entries(mut self, max_entries: u32) -> Self {
        self.max_index_entries = max_entries;
        self
    }

    pub fn retention_period(mut self, period: Duration) -> Self {
        self.retention_period = period;
        self
    }

    pub fn cleaner_interval(mut self, interval: Duration) -> Self {
        self.cleaner_interval = interval;
        self
    }

    pub fn flush_policy(mut self, policy: FlushPolicy) -> Self {
        self.flush_policy = policy;
        self
    }
}
