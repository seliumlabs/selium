use anyhow::Result;
use bytes::Bytes;
use lz4_flex::frame::FrameDecoder;
use selium_traits::compression::Decompress;
use std::io::Read;

#[derive(Debug)]
pub struct Lz4Decomp;

impl Decompress for Lz4Decomp {
    fn decompress(&self, input: Bytes) -> Result<Bytes> {
        let mut buf = Vec::new();
        let mut decoder = FrameDecoder::new(&input[..]);
        decoder.read_to_end(&mut buf)?;

        Ok(buf.into())
    }
}
