use anyhow::Result;
use bytes::Bytes;
use selium_traits::compression::Compress;

const HIGHEST_COMPRESSION: i32 = 9;
const BALANCED_COMPRESSION: i32 = 5;
const FASTEST_COMPRESSION: i32 = 1;

pub fn highest_ratio() -> ZstdComp {
    ZstdComp {
        level: HIGHEST_COMPRESSION,
    }
}

pub fn balanced() -> ZstdComp {
    ZstdComp {
        level: BALANCED_COMPRESSION,
    }
}

pub fn fastest() -> ZstdComp {
    ZstdComp {
        level: FASTEST_COMPRESSION,
    }
}

pub fn level(level: i32) -> ZstdComp {
    ZstdComp { level }
}

pub fn default() -> ZstdComp {
    ZstdComp {
        level: zstd::DEFAULT_COMPRESSION_LEVEL,
    }
}

pub struct ZstdComp {
    level: i32,
}

impl Compress for ZstdComp {
    fn compress(&self, input: Bytes) -> Result<Bytes> {
        let output = zstd::encode_all(&input[..], self.level)?;
        Ok(output.into())
    }
}
