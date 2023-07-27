use crate::traits::TryIntoU64;
use crate::Operation;
use anyhow::Result;

pub const KEEP_ALIVE_DEFAULT: u64 = 5;
pub const RETENTION_POLICY_DEFAULT: u64 = 0;

#[derive(Debug)]
pub struct ClientBuilder<T> {
    pub state: T,
}

#[derive(Debug)]
pub struct ClientCommon {
    pub topic: String,
    pub keep_alive: u64,
    pub retention_policy: u64,
    pub operations: Vec<Operation>,
}

impl ClientCommon {
    pub fn new(topic: &str) -> Self {
        Self {
            topic: topic.to_owned(),
            keep_alive: KEEP_ALIVE_DEFAULT,
            retention_policy: RETENTION_POLICY_DEFAULT,
            operations: Vec::new(),
        }
    }

    pub fn map(&mut self, module_path: &str) {
        self.operations.push(Operation::Map(module_path.into()));
    }

    pub fn filter(&mut self, module_path: &str) {
        self.operations.push(Operation::Filter(module_path.into()));
    }

    pub fn keep_alive<T: TryIntoU64>(&mut self, interval: T) -> Result<()> {
        self.keep_alive = interval.try_into_u64()?;
        Ok(())
    }

    pub fn retain<T: TryIntoU64>(&mut self, policy: T) -> Result<()> {
        self.retention_policy = policy.try_into_u64()?;
        Ok(())
    }
}
