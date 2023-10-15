//! Support for brotli, a lossless data compression algorithm developed by Google.
//!
//! Adapts the [brotli] crate, a popular, stable implementation of the brotli compression algorithm
//! built in Rust for use with Selium.

mod comp;
mod decomp;

pub use comp::*;
pub use decomp::*;
