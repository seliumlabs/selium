use super::types::DeflateLibrary;
use anyhow::Result;
use bytes::Bytes;
use flate2::read::{GzDecoder, ZlibDecoder};
use selium_traits::compression::Decompress;
use std::io::Read;

#[derive(Default, Clone)]
pub struct DeflateDecomp {
    library: DeflateLibrary,
}

impl DeflateDecomp {
    pub fn new(library: DeflateLibrary) -> Self {
        Self { library }
    }

    pub fn gzip() -> Self {
        Self::new(DeflateLibrary::Gzip)
    }

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
