use crate::traits::compression::Decompress;
use anyhow::Result;
use bytes::Bytes;

#[derive(Debug)]
pub struct ZstdDecomp;

impl Decompress for ZstdDecomp {
    fn decompress(&self, input: Bytes) -> Result<Bytes> {
        let output = zstd::decode_all(&input[..])?;
        Ok(output.into())
    }
}
