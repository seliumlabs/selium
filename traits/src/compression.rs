use bytes::Bytes;
use anyhow::Result;

pub trait Compress {
    fn compress(&self, input: Bytes) -> Result<Bytes>;
}

pub trait Decompress {
    fn decompress(&self, input: Bytes) -> Result<Bytes>;
}
