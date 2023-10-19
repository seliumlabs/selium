use super::types::DeflateLibrary;
use crate::traits::compression::{Compress, CompressionLevel};
use bytes::Bytes;
use flate2::write::{GzEncoder, ZlibEncoder};
use flate2::Compression;
use std::io::Write;

/// Compression half of DEFLATE implementation.
///
/// `DeflateComp` implements [Compress], and can be constructed for use with a `Publisher` stream.
#[derive(Default)]
pub struct DeflateComp {
    library: DeflateLibrary,
    level: Compression,
}

impl DeflateComp {
    /// Constructs a new `DeflateComp` instance, using the provided [DeflateLibrary] variant. This
    /// constructor is used directly by the `gzip` and `zlib` associated functions, so it is recommended to use
    /// either of those to construct an instance.
    pub fn new(library: DeflateLibrary) -> Self {
        Self {
            library,
            level: Compression::default(),
        }
    }

    /// Constructs a new `DeflateComp` instance, using `gzip`as the preferred implementation of the DEFLATE
    /// algorithm.
    pub fn gzip() -> Self {
        DeflateComp::new(DeflateLibrary::Gzip)
    }

    /// Constructs a new `DeflateComp` instance, using `zlib` as the preferred implementation of the DEFLATE
    /// algorithm.
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
    fn compress(&self, input: Bytes) -> anyhow::Result<Bytes> {
        let bytes = match self.library {
            DeflateLibrary::Gzip => {
                let mut encoder = GzEncoder::new(vec![], self.level);
                encoder.write_all(&input)?;
                encoder.finish()?
            }
            DeflateLibrary::Zlib => {
                let mut encoder = ZlibEncoder::new(vec![], self.level);
                encoder.write_all(&input)?;
                encoder.finish()?
            }
        };

        Ok(bytes.into())
    }
}
