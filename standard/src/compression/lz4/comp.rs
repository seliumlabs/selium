use anyhow::Result;
use bytes::Bytes;
use lz4_flex::frame::FrameEncoder;
use selium_traits::compression::Compress;
use std::io::Write;

#[derive(Debug, Clone)]
pub struct Lz4Comp;

impl Compress for Lz4Comp {
    fn compress(&self, input: Bytes) -> Result<Bytes> {
        let mut encoder = FrameEncoder::new(vec![]);
        encoder.write_all(&input)?;

        Ok(encoder.finish()?.into())
    }
}
