use anyhow::Result;
use brotli2::read::BrotliDecoder;
use bytes::Bytes;
use selium_traits::compression::Decompress;
use std::io::Read;

#[derive(Debug, Clone)]
pub struct BrotliDecomp;

impl Decompress for BrotliDecomp {
    fn decompress(&self, input: Bytes) -> Result<Bytes> {
        let mut buf = Vec::new();
        BrotliDecoder::new(&input[..]).read_to_end(&mut buf)?;

        Ok(buf.into())
    }
}
