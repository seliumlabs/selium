use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

pub type SharedLogConfig = Arc<LogConfig>;

#[derive(Debug)]
pub struct LogConfig {
    max_index_entries: u32,
    segments_path: PathBuf,
    retention_period: Duration,
    cleaner_interval: Duration,
}

impl LogConfig {
    pub fn new(
        max_index_entries: u32,
        segments_path: impl AsRef<Path>,
        retention_period: Duration,
        cleaner_interval: Duration,
    ) -> Self {
        Self {
            max_index_entries,
            segments_path: segments_path.as_ref().to_owned(),
            retention_period,
            cleaner_interval,
        }
    }

    pub fn max_index_entries(&self) -> u32 {
        self.max_index_entries
    }

    pub fn segments_path(&self) -> &Path {
        &self.segments_path
    }

    pub fn retention_period(&self) -> Duration {
        self.retention_period
    }

    pub fn cleaner_interval(&self) -> Duration {
        self.cleaner_interval
    }
}
