use crate::traits::TryIntoU64;
use anyhow::Result;
use quinn::Connection;
use selium_common::types::Operation;

pub(crate) const RETENTION_POLICY_DEFAULT: u64 = 0;

#[derive(Debug)]
pub struct StreamBuilder<T> {
    pub(crate) state: T,
    pub(crate) connection: Connection,
}

#[doc(hidden)]
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

    #[doc(hidden)]
    pub fn map(&mut self, module_path: &str) {
        self.operations.push(Operation::Map(module_path.into()));
    }

    #[doc(hidden)]
    pub fn filter(&mut self, module_path: &str) {
        self.operations.push(Operation::Filter(module_path.into()));
    }

    #[doc(hidden)]
    pub fn retain<T: TryIntoU64>(&mut self, policy: T) -> Result<()> {
        self.retention_policy = policy.try_into_u64()?;
        Ok(())
    }
}
