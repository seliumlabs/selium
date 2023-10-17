use crate::traits::compression::Compress;
use anyhow::Result;
use bytes::Bytes;
use lz4_flex::frame::FrameEncoder;
use std::io::Write;

/// Compression half of lz4 implementation.
///
/// `Lz4Comp` implements [Compress], and can be constructed for use with a `Publisher` stream.
#[derive(Debug)]
pub struct Lz4Comp;

impl Compress for Lz4Comp {
    fn compress(&self, input: Bytes) -> Result<Bytes> {
        let mut encoder = FrameEncoder::new(vec![]);
        encoder.write_all(&input)?;

        Ok(encoder.finish()?.into())
    }
}
