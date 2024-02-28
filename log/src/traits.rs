use crate::{index::IndexEntry, message::Message};
use anyhow::Result;
use async_trait::async_trait;
use std::ops::Range;

pub trait MmapCommon {
    fn find<F: Fn(&IndexEntry) -> bool>(&self, f: F) -> Option<IndexEntry>;
    fn get_next_offset(&self) -> u32;
}

#[async_trait]
pub trait SegmentCommon {
    async fn read_slice(&self, offset_range: Range<u64>) -> Result<Vec<Message>>;
}
