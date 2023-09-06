use super::types::DeflateLibrary;
use selium_traits::compression::Decompress;
use anyhow::Result;
use bytes::Bytes;
use flate2::read::{GzDecoder, ZlibDecoder};
use std::io::Read;

pub fn gzip() -> DeflateDecomp {
    DeflateDecomp {
        library: DeflateLibrary::Gzip,
    }
}

pub fn zlib() -> DeflateDecomp {
    DeflateDecomp {
        library: DeflateLibrary::Zlib,
    }
}

pub fn default() -> DeflateDecomp {
    DeflateDecomp {
        library: DeflateLibrary::default(),
    }
}

#[derive(Default)]
pub struct DeflateDecomp {
    library: DeflateLibrary,
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
