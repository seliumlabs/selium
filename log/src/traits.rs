use crate::index::IndexEntry;
use anyhow::Result;

pub trait MmapCommon {
    fn find<F: Fn(&IndexEntry) -> bool>(&self, f: F) -> Option<IndexEntry>;
    fn get_next_offset(&self) -> Result<u32>;
}
