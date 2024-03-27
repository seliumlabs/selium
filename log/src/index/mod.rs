mod entry;
mod mmap;

use crate::{config::SharedLogConfig, error::Result};
pub use entry::IndexEntry;
use mmap::Mmap;
use std::path::Path;

#[derive(Debug)]
pub struct Index {
    mmap: Mmap,
    current_offset: u32,
    config: SharedLogConfig,
}

impl Index {
    pub fn new(mmap: Mmap, current_offset: u32, config: SharedLogConfig) -> Self {
        Self {
            mmap,
            current_offset,
            config,
        }
    }

    pub async fn open(path: impl AsRef<Path>, config: SharedLogConfig) -> Result<Self> {
        let mmap = Mmap::load(path).await?;
        let next_offset = mmap.get_current_offset();
        Ok(Self::new(mmap, next_offset, config))
    }

    pub async fn create(path: impl AsRef<Path>, config: SharedLogConfig) -> Result<Self> {
        let mmap = Mmap::create(path, config.clone()).await?;
        Ok(Self::new(mmap, 0, config))
    }

    pub async fn append(&mut self, timestamp: u64, file_position: u64) -> Result<()> {
        if self.current_offset <= self.config.max_index_entries() {
            let next_offset = self.current_offset + 1;
            let entry = IndexEntry::new(next_offset, timestamp, file_position);
            self.mmap.push(entry);
            self.mmap.flush()?;
            self.current_offset = next_offset;
        }

        Ok(())
    }

    pub fn lookup(&self, relative_offset: u32) -> Result<Option<IndexEntry>> {
        if relative_offset > self.current_offset {
            return Ok(None);
        }

        let entry = self
            .mmap
            .find(|entry| entry.relative_offset() == relative_offset);

        Ok(entry)
    }

    pub async fn remove(self) -> Result<()> {
        self.mmap.remove().await?;
        Ok(())
    }

    pub fn current_offset(&self) -> u32 {
        self.current_offset
    }

    pub fn is_full(&self) -> bool {
        self.current_offset == self.config.max_index_entries()
    }
}
