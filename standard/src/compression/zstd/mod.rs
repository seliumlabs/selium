//! Support for zstd, a [lossless data compression algorithm developed by
//! Facebook](https://github.com/facebook/zstd).
//!
//! Adapts the [zstd] crate, a stable implementation of the zstd compression algorithm
//! built in Rust for use with `Selium`.
mod comp;
mod decomp;

pub use comp::*;
pub use decomp::*;
