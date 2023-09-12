use anyhow::Result;
use brotli2::{write::BrotliEncoder, CompressMode, CompressParams};
use bytes::Bytes;
use selium_traits::compression::{Compress, CompressionLevel};
use std::io::Write;

const HIGHEST_COMPRESSION: u32 = 9;
const RECOMMENDED_COMPRESSION: u32 = 6;
const FASTEST_COMPRESSION: u32 = 1;

#[derive(Debug, Clone)]
pub struct BrotliComp {
    params: CompressParams,
}

impl Default for BrotliComp {
    fn default() -> Self {
        let mut params = CompressParams::new();

        params
            .mode(CompressMode::Generic)
            .quality(RECOMMENDED_COMPRESSION);

        Self { params }
    }
}

impl BrotliComp {
    pub fn new(mode: CompressMode) -> Self {
        let mut params = CompressParams::new();
        params.mode(mode);
        Self { params }
    }

    pub fn generic() -> Self {
        BrotliComp::new(CompressMode::Generic)
    }

    pub fn text() -> Self {
        BrotliComp::new(CompressMode::Text)
    }

    pub fn font() -> Self {
        BrotliComp::new(CompressMode::Font)
    }
}

impl CompressionLevel for BrotliComp {
    fn highest_ratio(mut self) -> Self {
        self.params.quality(HIGHEST_COMPRESSION);
        self
    }

    fn balanced(mut self) -> Self {
        self.params.quality(RECOMMENDED_COMPRESSION);
        self
    }

    fn fastest(mut self) -> Self {
        self.params.quality(FASTEST_COMPRESSION);
        self
    }

    fn level(mut self, level: u32) -> Self {
        self.params.quality(level);
        self
    }
}

impl Compress for BrotliComp {
    fn compress(&self, mut input: Bytes) -> Result<Bytes> {
        let mut encoder = BrotliEncoder::from_params(vec![], &self.params);
        encoder.write_all(&mut input)?;

        Ok(encoder.finish()?.into())
    }
}
