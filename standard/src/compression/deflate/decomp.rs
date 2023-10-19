use super::types::DeflateLibrary;
use crate::traits::compression::Decompress;
use anyhow::Result;
use bytes::Bytes;
use flate2::read::{GzDecoder, ZlibDecoder};
use std::io::Read;

/// Decompression half of DEFLATE implementation.
///
/// `DeflateDecomp` implements [Decompress], and can be constructed for use with a `Subscriber` stream.
#[derive(Default)]
pub struct DeflateDecomp {
    library: DeflateLibrary,
}

impl DeflateDecomp {
    /// Constructs a new `DeflateDecomp` instance, using the provided [DeflateLibrary] variant. This
    /// constructor is used directly by the `gzip` and `zlib` associated functions, so it is recommended to use
    /// either of those to construct an instance.
    pub fn new(library: DeflateLibrary) -> Self {
        Self { library }
    }

    /// Constructs a new `DeflateDecomp` instance, using [gzip](https://gzip.org) as the preferred
    /// implementation of the DEFLATE algorithm.
    pub fn gzip() -> Self {
        Self::new(DeflateLibrary::Gzip)
    }

    /// Constructs a new `DeflateDecomp` instance, using [zlib](https://github.com/madler/zlib) as the preferred
    /// implementation of the DEFLATE algorithm.
    pub fn zlib() -> DeflateDecomp {
        Self::new(DeflateLibrary::Zlib)
    }
}

impl Decompress for DeflateDecomp {
    fn decompress(&self, input: Bytes) -> Result<Bytes> {
        let mut output = Vec::new();

        match self.library {
            DeflateLibrary::Gzip => {
                let mut decoder = GzDecoder::new(&input[..]);
                decoder.read_to_end(&mut output)?;
            }
            DeflateLibrary::Zlib => {
                let mut decoder = ZlibDecoder::new(&input[..]);
                decoder.read_to_end(&mut output)?;
            }
        };

        Ok(output.into())
    }
}
