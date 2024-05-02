use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum Offset {
    FromBeginning(u64),
    FromEnd(u64),
}

impl Default for Offset {
    fn default() -> Self {
        Self::FromEnd(0)
    }
}

impl From<u64> for Offset {
    fn from(value: u64) -> Self {
        Self::FromBeginning(value)
    }
}
