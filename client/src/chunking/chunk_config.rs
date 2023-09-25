use crate::utils::units;

const MAX_CHUNK_SIZE: u64 = units::megabyte(1);

#[derive(Debug, Clone)]
pub struct ChunkConfig {
    pub(crate) chunk_size: u64,
}

impl Default for ChunkConfig {
    fn default() -> Self {
        Self::max()
    }
}

impl ChunkConfig {
    pub fn new(chunk_size: u64) -> Self {
        assert!(chunk_size < MAX_CHUNK_SIZE);

        Self {
            chunk_size,
        }
    }

    pub fn max() -> Self {
        Self::new(MAX_CHUNK_SIZE)
    }

    pub fn medium() -> Self {
        Self::new(units::kilobyte(512))
    }

    pub fn small() -> Self {
        Self::new(units::kilobyte(256))
    }
}
