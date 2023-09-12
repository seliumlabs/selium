use anyhow::Result;
use bytes::Bytes;

pub trait Compress {
    fn compress(&self, input: Bytes) -> Result<Bytes>;
}

pub trait Decompress {
    fn decompress(&self, input: Bytes) -> Result<Bytes>;
}

pub trait CompressionLevel {
    fn highest_ratio(self) -> Self;
    fn balanced(self) -> Self;
    fn fastest(self) -> Self;
    fn level(self, level: u32) -> Self;
}
