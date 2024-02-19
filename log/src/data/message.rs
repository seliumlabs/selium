use bytes::Bytes;
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub struct Message {
    timestamp: u64,
    records: Vec<Bytes>,
}

impl Message {
    pub fn new(timestamp: u64, records: &[Bytes]) -> Self {
        Self {
            timestamp,
            records: records.to_owned(),
        }
    }

    pub fn timestamp(&self) -> u64 {
        self.timestamp
    }
}
