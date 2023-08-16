use serde::{Deserialize, Serialize};

#[doc(hidden)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Executor {
    Rhai(String),
    Wasm(String),
}

#[doc(hidden)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Operation {
    Map(Executor),
    Filter(Executor),
}
