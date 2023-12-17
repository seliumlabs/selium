use std::pin::Pin;

use futures::Sink;

pub mod args;
#[cfg(feature = "__cloud")]
mod cloud;
pub mod quic;
pub mod server;
pub mod sink;
pub mod topic;

type BoxSink<T, E> = Pin<Box<dyn Sink<T, Error = E> + Send>>;
