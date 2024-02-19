use super::entry::SIZE_OF_INDEX_ENTRY;
use crate::{index::IndexEntry, traits::MmapCommon};
use anyhow::{anyhow, Result};
use bytes::Buf;
use std::{ops::Deref, path::Path};
use tokio::fs::OpenOptions;

#[derive(Debug)]
pub struct Mmap(memmap2::Mmap);

impl Mmap {
    pub async fn load(path: impl AsRef<Path>) -> Result<Self> {
        let file = OpenOptions::new().read(true).open(path).await?;
        let mmap = unsafe { memmap2::Mmap::map(&file)? };
        Ok(Self(mmap))
    }
}

impl MmapCommon for Mmap {
    fn find<F: Fn(&IndexEntry) -> bool>(&self, f: F) -> Option<IndexEntry> {
        find(&self.0, f)
    }

    fn get_next_offset(&self) -> Result<u32> {
        get_next_offset(&self.0)
    }
}

impl Deref for Mmap {
    type Target = memmap2::Mmap;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

#[derive(Debug)]
pub struct MmapMut(memmap2::MmapMut);

impl MmapMut {
    pub async fn load(path: impl AsRef<Path>) -> Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(path)
            .await?;

        let mmap = unsafe { memmap2::MmapMut::map_mut(&file)? };
        Ok(Self(mmap))
    }

    pub fn push(&mut self, entry: IndexEntry) {
        let slice_start = entry.relative_offset() as usize * SIZE_OF_INDEX_ENTRY;
        let slice_end = slice_start + SIZE_OF_INDEX_ENTRY;
        self.0[slice_start..slice_end].copy_from_slice(&entry.into_slice());
    }
}

impl Deref for MmapMut {
    type Target = memmap2::MmapMut;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl MmapCommon for MmapMut {
    fn find<F: Fn(&IndexEntry) -> bool>(&self, f: F) -> Option<IndexEntry> {
        find(&self.0, f)
    }

    fn get_next_offset(&self) -> Result<u32> {
        get_next_offset(&self.0)
    }
}

fn find<F: Fn(&IndexEntry) -> bool>(mmap: &[u8], f: F) -> Option<IndexEntry> {
    for relative_offset in get_offset_range(mmap) {
        let index_pos = relative_offset * SIZE_OF_INDEX_ENTRY;
        let slice = get_entry_slice(mmap, index_pos);
        let entry = IndexEntry::from_slice(slice);

        if f(&entry) {
            return Some(entry);
        };
    }

    None
}

fn get_next_offset(mmap: &[u8]) -> Result<u32> {
    // If the memory mapped file is empty, there are no relative offsets in the index yet.
    if mmap.is_empty() {
        return Ok(1);
    }

    for i in get_offset_range(mmap) {
        let offset = i * SIZE_OF_INDEX_ENTRY;
        let mut slice = get_entry_slice(mmap, offset);

        if slice.get_u64() == 0 {
            return Ok(i as u32);
        }
    }

    Err(anyhow!("Unable to find free offset in index"))
}

fn get_entry_slice(mmap: &[u8], offset: usize) -> &[u8] {
    let length = offset + SIZE_OF_INDEX_ENTRY;
    &mmap[offset..length]
}

fn get_offset_range(mmap: &[u8]) -> std::ops::Range<usize> {
    0..mmap.len() / SIZE_OF_INDEX_ENTRY
}
