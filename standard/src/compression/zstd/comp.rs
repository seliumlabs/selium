use crate::traits::compression::{Compress, CompressionLevel};
use anyhow::Result;
use bytes::Bytes;

/// Highest compression level available for zstd.
pub const HIGHEST_COMPRESSION: i32 = 9;

/// Recommended compression level for zstd.
pub const RECOMMENDED_COMPRESSION: i32 = zstd::DEFAULT_COMPRESSION_LEVEL;

/// Fastest compression level available for zstd.
pub const FASTEST_COMPRESSION: i32 = 1;

/// Compression half of zstd implementation.
///
/// `ZstdComp` implements [Compress], and can be constructed for use with a `Publisher` stream.
#[derive(Debug)]
pub struct ZstdComp {
    level: i32,
}

impl Default for ZstdComp {
    fn default() -> Self {
        ZstdComp::new()
    }
}

impl ZstdComp {
    pub fn new() -> Self {
        ZstdComp {
            level: RECOMMENDED_COMPRESSION,
        }
    }
}

impl CompressionLevel for ZstdComp {
    fn highest_ratio(mut self) -> Self {
        self.level = HIGHEST_COMPRESSION;
        self
    }

    fn balanced(mut self) -> Self {
        self.level = RECOMMENDED_COMPRESSION;
        self
    }

    fn fastest(mut self) -> Self {
        self.level = FASTEST_COMPRESSION;
        self
    }

    fn level(mut self, level: u32) -> Self {
        self.level = level.try_into().unwrap();
        self
    }
}

impl Compress for ZstdComp {
    fn compress(&self, input: Bytes) -> Result<Bytes> {
        let output = zstd::encode_all(&input[..], self.level)?;
        Ok(output.into())
    }
}
