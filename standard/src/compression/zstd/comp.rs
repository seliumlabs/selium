use anyhow::Result;
use bytes::Bytes;
use selium_traits::compression::{Compress, CompressionLevel};

const HIGHEST_COMPRESSION: i32 = 9;
const FASTEST_COMPRESSION: i32 = 1;

pub struct InitialState;

pub struct ConfiguredState {
    level: i32,
}

impl ConfiguredState {
    pub fn new(level: i32) -> Self {
        Self { level }
    }
}

impl Default for ConfiguredState {
    fn default() -> Self {
        Self {
            level: zstd::DEFAULT_COMPRESSION_LEVEL,
        }
    }
}

pub fn new() -> ZstdComp<InitialState> {
    ZstdComp {
        state: InitialState,
    }
}

pub fn default() -> ZstdComp<ConfiguredState> {
    ZstdComp::default()
}

#[derive(Default)]
pub struct ZstdComp<State> {
    state: State,
}

impl CompressionLevel for ZstdComp<InitialState> {
    type Target = ZstdComp<ConfiguredState>;

    fn highest_ratio(self) -> Self::Target {
        ZstdComp {
            state: ConfiguredState::new(HIGHEST_COMPRESSION),
        }
    }

    fn balanced(self) -> Self::Target {
        ZstdComp {
            state: ConfiguredState::new(zstd::DEFAULT_COMPRESSION_LEVEL),
        }
    }

    fn fastest(self) -> Self::Target {
        ZstdComp {
            state: ConfiguredState::new(FASTEST_COMPRESSION),
        }
    }

    fn level(self, level: u32) -> Self::Target {
        let level = level.try_into().unwrap();

        ZstdComp {
            state: ConfiguredState::new(level),
        }
    }
}

impl Compress for ZstdComp<ConfiguredState> {
    fn compress(&self, input: Bytes) -> Result<Bytes> {
        let output = zstd::encode_all(&input[..], self.state.level)?;
        Ok(output.into())
    }
}
