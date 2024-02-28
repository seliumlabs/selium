use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

pub type SharedLogConfig = Arc<LogConfig>;

#[derive(Debug)]
pub struct LogConfig {
    max_index_entries: u32,
    segments_path: PathBuf,
}

impl LogConfig {
    pub fn new(max_index_entries: u32, segments_path: impl AsRef<Path>) -> Self {
        Self {
            max_index_entries,
            segments_path: segments_path.as_ref().to_owned(),
        }
    }

    pub fn max_index_entries(&self) -> u32 {
        self.max_index_entries
    }

    pub fn segments_path(&self) -> &Path {
        &self.segments_path
    }
}
