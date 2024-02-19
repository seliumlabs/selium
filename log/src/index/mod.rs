mod entry;
mod mmap;

pub use entry::IndexEntry;

use crate::{config::SharedLogConfig, traits::MmapCommon};
use anyhow::Result;
use mmap::{Mmap, MmapMut};
use std::path::Path;

#[derive(Debug)]
pub struct Index {
    mmap: Mmap,
    next_offset: u32,
    config: SharedLogConfig,
}

impl Index {
    pub fn new(mmap: Mmap, next_offset: u32, config: SharedLogConfig) -> Self {
        Self {
            mmap,
            next_offset,
            config,
        }
    }

    pub async fn open(path: impl AsRef<Path>, config: SharedLogConfig) -> Result<Self> {
        let mmap = Mmap::load(path).await?;
        let next_offset = mmap.get_next_offset()?;
        Ok(Self::new(mmap, next_offset, config))
    }

    pub fn lookup(&self, relative_offset: u32) -> Result<Option<IndexEntry>> {
        if relative_offset > self.next_offset {
            return Ok(None);
        }

        let entry = self
            .mmap
            .find(|entry| entry.relative_offset() == relative_offset);

        Ok(entry)
    }
}

#[derive(Debug)]
pub struct MutIndex {
    mmap: MmapMut,
    next_offset: u32,
    config: SharedLogConfig,
}

impl MutIndex {
    pub fn new(mmap: MmapMut, next_offset: u32, config: SharedLogConfig) -> Self {
        Self {
            mmap,
            next_offset,
            config,
        }
    }

    pub async fn create(path: impl AsRef<Path>, config: SharedLogConfig) -> Result<Self> {
        let mmap = MmapMut::load(path).await?;
        Ok(Self::new(mmap, 0, config))
    }

    pub async fn open(path: impl AsRef<Path>, config: SharedLogConfig) -> Result<Self> {
        let mmap = MmapMut::load(path).await?;
        let next_offset = mmap.get_next_offset()?;
        Ok(Self::new(mmap, next_offset, config))
    }

    pub async fn append(&mut self, timestamp: u64, file_position: u64) -> Result<()> {
        if self.next_offset < self.config.max_index_entries() {
            let entry = IndexEntry::new(self.next_offset, timestamp, file_position);
            self.mmap.push(entry);
            self.mmap.flush()?;
            self.next_offset += 1;
        }

        Ok(())
    }

    pub fn lookup(&self, relative_offset: u32) -> Result<Option<IndexEntry>> {
        if relative_offset > self.next_offset {
            return Ok(None);
        }

        let entry = self
            .mmap
            .find(|entry| entry.relative_offset() == relative_offset);

        Ok(entry)
    }
}
