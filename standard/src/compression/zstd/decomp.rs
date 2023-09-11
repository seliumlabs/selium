use anyhow::Result;
use bytes::Bytes;
use selium_traits::compression::Decompress;

pub fn new() -> ZstdDecomp {
    ZstdDecomp
}

pub struct ZstdDecomp;

impl Decompress for ZstdDecomp {
    fn decompress(&self, input: Bytes) -> Result<Bytes> {
        let output = zstd::decode_all(&input[..])?;
        Ok(output.into())
    }
}
