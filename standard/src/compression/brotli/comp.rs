use anyhow::Result;
use brotli2::{write::BrotliEncoder, CompressMode, CompressParams};
use bytes::Bytes;
use selium_traits::compression::{Compress, CompressionLevel};
use std::io::Write;

const HIGHEST_COMPRESSION: u32 = 9;
const RECOMMENDED_COMPRESSION: u32 = 6;
const FASTEST_COMPRESSION: u32 = 1;

pub struct InitialState {
    mode: CompressMode,
}

impl InitialState {
    pub fn new(mode: CompressMode) -> Self {
        Self { mode }
    }
}

pub struct ConfiguredState {
    params: CompressParams,
}

impl ConfiguredState {
    pub fn new(mode: CompressMode, quality: u32) -> Self {
        Self {
            params: CompressParams::new().mode(mode).quality(quality).to_owned(),
        }
    }
}

impl Default for ConfiguredState {
    fn default() -> Self {
        Self::new(CompressMode::Generic, RECOMMENDED_COMPRESSION)
    }
}

pub fn generic() -> BrotliComp<InitialState> {
    BrotliComp {
        state: InitialState::new(CompressMode::Generic),
    }
}

pub fn text() -> BrotliComp<InitialState> {
    BrotliComp {
        state: InitialState::new(CompressMode::Text),
    }
}

pub fn font() -> BrotliComp<InitialState> {
    BrotliComp {
        state: InitialState::new(CompressMode::Font),
    }
}

pub fn default() -> BrotliComp<ConfiguredState> {
    BrotliComp::default()
}

#[derive(Default)]
pub struct BrotliComp<State> {
    state: State,
}

impl CompressionLevel for BrotliComp<InitialState> {
    type Target = BrotliComp<ConfiguredState>;

    fn highest_ratio(self) -> Self::Target {
        BrotliComp {
            state: ConfiguredState::new(self.state.mode, HIGHEST_COMPRESSION),
        }
    }

    fn balanced(self) -> Self::Target {
        BrotliComp {
            state: ConfiguredState::new(self.state.mode, RECOMMENDED_COMPRESSION),
        }
    }

    fn fastest(self) -> Self::Target {
        BrotliComp {
            state: ConfiguredState::new(self.state.mode, FASTEST_COMPRESSION),
        }
    }

    fn level(self, level: u32) -> Self::Target {
        BrotliComp {
            state: ConfiguredState::new(self.state.mode, level),
        }
    }
}

impl Compress for BrotliComp<ConfiguredState> {
    fn compress(&self, mut input: Bytes) -> Result<Bytes> {
        let buf = Vec::new();
        let mut encoder = BrotliEncoder::from_params(buf, &self.state.params);
        encoder.write_all(&mut input)?;

        Ok(encoder.finish()?.into())
    }
}
