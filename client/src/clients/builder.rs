use anyhow::Result;

use crate::{traits::IntoTimestamp, Operation};

pub const KEEP_ALIVE_DEFAULT: u64 = 5;

#[derive(Debug)]
pub struct ClientBuilder<T> {
    pub state: T,
}

#[derive(Debug)]
pub struct ClientCommon {
    pub topic: String,
    pub keep_alive: u64,
    pub operations: Vec<Operation>,
}

impl ClientCommon {
    pub fn new(topic: &str) -> Self {
        Self {
            topic: topic.to_owned(),
            keep_alive: KEEP_ALIVE_DEFAULT,
            operations: Vec::new(),
        }
    }

    pub fn map(&mut self, module_path: &str) {
        self.operations.push(Operation::Map(module_path.into()));
    }

    pub fn filter(&mut self, module_path: &str) {
        self.operations.push(Operation::Filter(module_path.into()));
    }

    pub fn keep_alive<T: IntoTimestamp>(&mut self, interval: T) -> Result<()> {
        self.keep_alive = interval.into_timestamp()?;
        Ok(())
    }
}
