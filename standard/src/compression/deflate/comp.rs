use super::types::DeflateLibrary;
use bytes::Bytes;
use flate2::write::{GzEncoder, ZlibEncoder};
use flate2::Compression;
use selium_traits::compression::{Compress, CompressionLevel};
use std::io::Write;

#[derive(Default, Clone)]
pub struct DeflateComp {
    library: DeflateLibrary,
    level: Compression,
}

impl DeflateComp {
    pub fn new(library: DeflateLibrary) -> Self {
        Self {
            library,
            level: Compression::default(),
        }
    }

    pub fn gzip() -> Self {
        DeflateComp::new(DeflateLibrary::Gzip)
    }

    pub fn zlib() -> Self {
        DeflateComp::new(DeflateLibrary::Zlib)
    }
}

impl CompressionLevel for DeflateComp {
    fn highest_ratio(mut self) -> Self {
        self.level = Compression::best();
        self
    }

    fn balanced(mut self) -> Self {
        self.level = Compression::default();
        self
    }

    fn fastest(mut self) -> Self {
        self.level = Compression::fast();
        self
    }

    fn level(mut self, level: u32) -> Self {
        self.level = Compression::new(level);
        self
    }
}

impl Compress for DeflateComp {
    fn compress(&self, mut input: Bytes) -> anyhow::Result<Bytes> {
        let bytes = match self.library {
            DeflateLibrary::Gzip => {
                let mut encoder = GzEncoder::new(vec![], self.level);
                encoder.write(&mut input)?;
                encoder.finish()?
            }
            DeflateLibrary::Zlib => {
                let mut encoder = ZlibEncoder::new(vec![], self.level);
                encoder.write(&mut input)?;
                encoder.finish()?
            }
        };

        Ok(bytes.into())
    }
}
