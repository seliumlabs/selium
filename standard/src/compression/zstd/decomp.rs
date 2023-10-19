use crate::traits::compression::Decompress;
use anyhow::Result;
use bytes::Bytes;

/// Decompression half of zstd implementation.
///
/// `ZstdDecomp` implements [Decompress], and can be constructed for use with a `Subscriber` stream.
#[derive(Debug)]
pub struct ZstdDecomp;

impl Decompress for ZstdDecomp {
    fn decompress(&self, input: Bytes) -> Result<Bytes> {
        let output = zstd::decode_all(&input[..])?;
        Ok(output.into())
    }
}
