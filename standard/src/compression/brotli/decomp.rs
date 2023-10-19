use crate::traits::compression::Decompress;
use anyhow::Result;
use brotli::Decompressor;
use bytes::Bytes;
use std::io::Read;

const BUFFER_SIZE: usize = 4096;

/// Decompression half of Brotli implementation.
///
/// `BrotliDecomp` implements [Decompress], and can be constructed for use with a `Subscriber`
/// stream.
#[derive(Debug)]
pub struct BrotliDecomp;

impl Decompress for BrotliDecomp {
    fn decompress(&self, input: Bytes) -> Result<Bytes> {
        let mut buf = Vec::new();
        let mut decoder = Decompressor::new(&input[..], BUFFER_SIZE);
        decoder.read_to_end(&mut buf)?;

        Ok(buf.into())
    }
}
