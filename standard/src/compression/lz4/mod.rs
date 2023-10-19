//! Support for lz4, a [lossless data compression algorithm](https://github.com/lz4/lz4) that prioritizes
//! comp/decomp speed over compression ratio.
//!
//! Adapts the [lz4_flex] crate, a performant implementation of the lz4 compression algorithm built
//! in Rust for use with `Selium`.

mod comp;
mod decomp;

pub use comp::*;
pub use decomp::*;
