use anyhow::Result;
use brotli::Decompressor;
use bytes::Bytes;
use selium_traits::compression::Decompress;
use std::io::Read;

const BUFFER_SIZE: usize = 4096;

#[derive(Debug, Clone)]
pub struct BrotliDecomp;

impl Decompress for BrotliDecomp {
    fn decompress(&self, input: Bytes) -> Result<Bytes> {
        let mut buf = Vec::new();
        let mut decoder = Decompressor::new(&input[..], BUFFER_SIZE);
        decoder.read_to_end(&mut buf)?;

        Ok(buf.into())
    }
}
