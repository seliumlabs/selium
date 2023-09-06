use super::types::DeflateLibrary;
use bytes::Bytes;
use flate2::write::{GzEncoder, ZlibEncoder};
use flate2::Compression;
use selium_traits::compression::Compress;
use std::io::Write;

pub struct InitialState {
    library: DeflateLibrary,
}

#[derive(Default)]
pub struct ConfiguredState {
    library: DeflateLibrary,
    level: Compression,
}

pub fn gzip() -> DeflateComp<InitialState> {
    DeflateComp {
        state: InitialState {
            library: DeflateLibrary::Gzip,
        },
    }
}

pub fn zlib() -> DeflateComp<InitialState> {
    DeflateComp {
        state: InitialState {
            library: DeflateLibrary::Zlib,
        },
    }
}

pub fn default() -> DeflateComp<ConfiguredState> {
    DeflateComp {
        state: ConfiguredState::default(),
    }
}

pub struct DeflateComp<State> {
    state: State,
}

impl DeflateComp<InitialState> {
    pub fn highest_ratio(self) -> DeflateComp<ConfiguredState> {
        DeflateComp {
            state: ConfiguredState {
                library: self.state.library,
                level: Compression::best(),
            },
        }
    }

    pub fn balanced(self) -> DeflateComp<ConfiguredState> {
        DeflateComp {
            state: ConfiguredState {
                library: self.state.library,
                level: Compression::new(5),
            },
        }
    }

    pub fn fastest(self) -> DeflateComp<ConfiguredState> {
        DeflateComp {
            state: ConfiguredState {
                library: self.state.library,
                level: Compression::fast(),
            },
        }
    }

    pub fn level(self, level: u32) -> DeflateComp<ConfiguredState> {
        DeflateComp {
            state: ConfiguredState {
                library: self.state.library,
                level: Compression::new(level),
            },
        }
    }
}

impl Compress for DeflateComp<ConfiguredState> {
    fn compress(&self, mut input: Bytes) -> anyhow::Result<Bytes> {
        let mut buf = Vec::new();
        let level = self.state.level;

        match self.state.library {
            DeflateLibrary::Gzip => {
                let mut encoder = GzEncoder::new(&mut buf, level);
                encoder.write(&mut input)?;
            }
            DeflateLibrary::Zlib => {
                let mut encoder = ZlibEncoder::new(&mut buf, level);
                encoder.write(&mut input)?;
            }
        };

        Ok(buf.into())
    }
}
