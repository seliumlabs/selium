//! Data types and stream/sink wrappers to allow streams to recover from transient errors.
//!
//! By default, streams are wrapped in a `KeepAlive` type that will allow the stream to recover
//! from transient errors such as connection timeouts, scheduled server shutdowns, etc.
//!
//! In most cases, there is no input required from the user, as streams already enable this feature
//! with a default, reasonable connection retry strategy. However, if you wish to specify your own retry
//! strategy, you can do so by constructing a [BackoffStrategy] instance and providing it to the `Selium`
//! stream builder.

mod backoff_strategy;
mod connection_status;
mod helpers;
mod pubsub;
mod reqrep;

pub use backoff_strategy::*;
pub(crate) use connection_status::*;

pub use pubsub::*;
pub use reqrep::*;
