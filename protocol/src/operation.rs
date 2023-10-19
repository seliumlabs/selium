use serde::{Deserialize, Serialize};

#[doc(hidden)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Operation {
    Map(String),
    Filter(String),
}
