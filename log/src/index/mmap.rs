use super::entry::SIZE_OF_INDEX_ENTRY;
use crate::{
    config::SharedLogConfig,
    error::{LogError, Result},
    index::IndexEntry,
};
use bytes::Buf;
use std::{
    ops::{Deref, DerefMut},
    path::{Path, PathBuf},
};
use tokio::fs::{self, OpenOptions};

#[derive(Debug)]
pub struct Mmap {
    mmap: memmap2::MmapMut,
    path: PathBuf,
}

impl Mmap {
    pub async fn create(path: impl AsRef<Path>, config: SharedLogConfig) -> Result<Self> {
        let path = path.as_ref();

        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)
            .await?;

        let length = config.max_index_entries as u64 * SIZE_OF_INDEX_ENTRY as u64;
        file.set_len(length).await?;

        // Safety: https://docs.rs/memmap2/latest/memmap2/struct.Mmap.html#safety
        // Our usage is safe, as writes/flushes are performed atomically, and the appropriate filesystem
        // permissions are applied to the backed file on creation to prevent it from being modified by
        // outside processes.
        let mmap = unsafe { memmap2::MmapMut::map_mut(&file).map_err(LogError::MemoryMapIndex)? };

        Ok(Self {
            mmap,
            path: path.to_owned(),
        })
    }

    pub async fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let file = OpenOptions::new().write(true).read(true).open(path).await?;

        // Safety: See the Mmap::create method for explanation.
        let mmap = unsafe { memmap2::MmapMut::map_mut(&file).map_err(LogError::MemoryMapIndex)? };

        Ok(Self {
            mmap,
            path: path.to_owned(),
        })
    }

    pub async fn remove(self) -> Result<()> {
        fs::remove_file(&self.path).await?;
        Ok(())
    }

    pub fn push(&mut self, entry: IndexEntry) {
        let slice_start = (entry.relative_offset() - 1) as usize * SIZE_OF_INDEX_ENTRY;
        let slice_end = slice_start + SIZE_OF_INDEX_ENTRY;
        self.mmap[slice_start..slice_end].copy_from_slice(&entry.into_slice());
    }

    pub fn find<F: Fn(&IndexEntry) -> bool>(&self, f: F) -> Option<IndexEntry> {
        for relative_offset in self.get_offset_range() {
            let index_pos = relative_offset * SIZE_OF_INDEX_ENTRY;
            let slice = self.get_entry_slice(index_pos);
            let entry = IndexEntry::from_slice(slice);

            if f(&entry) {
                return Some(entry);
            }
        }

        None
    }

    pub fn get_current_offset(&self) -> u32 {
        let last_offset = self.get_last_offset();

        if last_offset != 0 {
            return last_offset;
        }

        for i in self.get_offset_range() {
            let offset = i * SIZE_OF_INDEX_ENTRY;
            let mut slice = self.get_entry_slice(offset);

            if slice.get_u32() == 0 {
                return i as u32;
            }
        }

        // If the memory mapped file is empty, there are no relative offsets in the index yet.
        1
    }

    fn get_last_offset(&self) -> u32 {
        let range_start = self.mmap.len() - SIZE_OF_INDEX_ENTRY;
        let mut slice = &self.mmap[range_start..];
        slice.get_u32()
    }

    fn get_entry_slice(&self, offset: usize) -> &[u8] {
        let length = offset + SIZE_OF_INDEX_ENTRY;
        &self.mmap[offset..length]
    }

    fn get_offset_range(&self) -> std::ops::Range<usize> {
        0..self.mmap.len() / SIZE_OF_INDEX_ENTRY
    }
}

impl Deref for Mmap {
    type Target = memmap2::MmapMut;

    fn deref(&self) -> &Self::Target {
        &self.mmap
    }
}

impl DerefMut for Mmap {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.mmap
    }
}
