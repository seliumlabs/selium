use bytes::Bytes;
use anyhow::Result;

pub trait Compress {
    fn compress(&self, input: Bytes) -> Result<Bytes>;
}

pub trait Decompress {
    fn decompress(&self, input: Bytes) -> Result<Bytes>;
}

pub trait CompressionLevel {
    type Target;

    fn highest_ratio(self) -> Self::Target;
    fn balanced(self) -> Self::Target;
    fn fastest(self) -> Self::Target;
    fn level(self, level: u32) -> Self::Target;
}

