use crate::traits::compression::{Compress, CompressionLevel};
use anyhow::Result;
use brotli::enc::backward_references::BrotliEncoderMode;
use brotli::enc::writer::CompressorWriter;
use brotli::enc::BrotliEncoderParams;
use bytes::Bytes;
use std::io::Write;

const HIGHEST_COMPRESSION: i32 = 11;
const RECOMMENDED_COMPRESSION: i32 = 6;
const FASTEST_COMPRESSION: i32 = 1;
const BUFFER_SIZE: usize = 4096;

#[derive(Debug, Default)]
pub struct BrotliComp {
    params: BrotliEncoderParams,
}

impl BrotliComp {
    pub fn new(mode: BrotliEncoderMode) -> Self {
        let params = BrotliEncoderParams {
            mode,
            ..Default::default()
        };

        Self { params }
    }

    pub fn generic() -> Self {
        BrotliComp::new(BrotliEncoderMode::BROTLI_MODE_GENERIC)
    }

    pub fn text() -> Self {
        BrotliComp::new(BrotliEncoderMode::BROTLI_MODE_TEXT)
    }

    pub fn font() -> Self {
        BrotliComp::new(BrotliEncoderMode::BROTLI_MODE_FONT)
    }
}

impl CompressionLevel for BrotliComp {
    fn highest_ratio(mut self) -> Self {
        self.params.quality = HIGHEST_COMPRESSION;
        self
    }

    fn balanced(mut self) -> Self {
        self.params.quality = RECOMMENDED_COMPRESSION;
        self
    }

    fn fastest(mut self) -> Self {
        self.params.quality = FASTEST_COMPRESSION;
        self
    }

    fn level(mut self, level: u32) -> Self {
        self.params.quality = level.try_into().unwrap();
        self
    }
}

impl Compress for BrotliComp {
    fn compress(&self, input: Bytes) -> Result<Bytes> {
        let mut encoder = CompressorWriter::with_params(vec![], BUFFER_SIZE, &self.params);
        encoder.write_all(&input)?;
        encoder.flush()?;

        Ok(encoder.into_inner().into())
    }
}
