use std::sync::Arc;

pub type SharedLogConfig = Arc<LogConfig>;

#[derive(Debug)]
pub struct LogConfig {
    max_index_entries: u32,
}

impl LogConfig {
    pub fn new(max_index_entries: u32) -> Self {
        Self { max_index_entries }
    }

    pub fn max_index_entries(&self) -> u32 {
        self.max_index_entries
    }
}
