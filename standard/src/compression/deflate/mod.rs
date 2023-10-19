//! Support for DEFLATE, a [lossless data compression algorithm](https://datatracker.ietf.org/doc/html/rfc1951).
//!
//! Adapts the [flate2] crate, the most widely used implementation of the DEFLATE compression algorithm
//! built in Rust for use with `Selium`.
//!
//! The `Selium Standard` support for DEFLATE includes two widely used implementations,
//! [gzip](https://gzip.org) and [zlib](https://github.com/madler/zlib).

mod comp;
mod decomp;
mod types;

pub use comp::*;
pub use decomp::*;
pub use types::*;
