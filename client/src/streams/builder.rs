use crate::traits::TryIntoU64;
use crate::Operation;
use anyhow::Result;
use quinn::Connection;

pub const RETENTION_POLICY_DEFAULT: u64 = 0;

#[derive(Debug)]
pub struct StreamBuilder<T> {
    pub(crate) state: T,
    pub(crate) connection: Connection,
}

#[derive(Debug)]
pub struct StreamCommon {
    pub(crate) topic: String,
    pub(crate) retention_policy: u64,
    pub(crate) operations: Vec<Operation>,
}

impl StreamCommon {
    pub fn new(topic: &str) -> Self {
        Self {
            topic: topic.to_owned(),
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

    pub fn retain<T: TryIntoU64>(&mut self, policy: T) -> Result<()> {
        self.retention_policy = policy.try_into_u64()?;
        Ok(())
    }
}
