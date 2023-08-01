//! Re-exports commonly used types and traits.
//!
//! Aside from conveniently re-exporting all traits required for client usage, the prelude may continue to expand
//! as the client API evolves, so it's encouraged to import the prelude to help alleviate any migration efforts as new
//! versions of the library are released.
//!
//! ```
//! use selium::prelude::*;
//! ```
//!

pub use crate::traits::{Open, StreamConfig};
