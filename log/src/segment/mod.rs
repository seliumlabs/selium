mod list;

use crate::config::SharedLogConfig;
use crate::data::Data;
use crate::error::Result;
use crate::index::Index;
use crate::message::{Message, MessageSlice};
pub use list::SegmentList;
use std::cmp;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Debug)]
pub struct Segment {
    index: Index,
    data: Data,
    base_offset: u64,
    end_offset: u64,
}

impl Segment {
    pub async fn open(base_offset: u64, config: SharedLogConfig) -> Result<Self> {
        let path = config.segments_path();
        let (index_path, data_path) = get_segment_paths(path, base_offset);
        let index = Index::open(index_path, config).await?;
        let data = Data::open(data_path).await?;
        let end_offset = base_offset + index.current_offset() as u64;

        Ok(Self {
            index,
            data,
            base_offset,
            end_offset,
        })
    }

    pub async fn create(base_offset: u64, config: SharedLogConfig) -> Result<Self> {
        let path = config.segments_path();
        let (index_path, data_path) = get_segment_paths(path, base_offset);
        let index = Index::create(index_path, config).await?;
        let data = Data::create(data_path).await?;

        Ok(Self {
            index,
            data,
            base_offset,
            end_offset: base_offset,
        })
    }

    pub async fn read_slice(&self, offset_range: Range<u64>) -> Result<MessageSlice> {
        let end_offset = cmp::min(offset_range.end, self.end_offset);
        let relative_start_offset = cmp::max(offset_range.start - self.base_offset, 1);
        let relative_end_offset = end_offset - self.base_offset;

        // TODO: Consider starting from relative offset 0 to avoid confusion and ambiguity.
        if let Some(start_entry) = self.index.lookup(relative_start_offset as u32)? {
            let start_pos = start_entry.physical_position();

            if end_offset == self.end_offset {
                let messages = self.data.read_messages(start_pos, None).await?;
                return Ok(MessageSlice::new(messages.as_slice(), end_offset + 1));
            }

            if let Some(end_entry) = self.index.lookup(relative_end_offset as u32)? {
                let end_pos = end_entry.physical_position();
                let messages = self.data.read_messages(start_pos, Some(end_pos)).await?;
                return Ok(MessageSlice::new(messages.as_slice(), end_offset));
            }
        }

        Ok(MessageSlice::empty(offset_range.start))
    }

    pub async fn write(&mut self, message: Message) -> Result<()> {
        let position = self.data.position();
        let timestamp = message.headers().timestamp();

        self.data.write(message).await?;
        self.index.append(timestamp, position).await?;
        self.end_offset += 1;

        Ok(())
    }

    pub async fn remove(self) -> Result<()> {
        self.data.remove().await?;
        self.index.remove().await?;
        Ok(())
    }

    pub fn is_stale(&self, stale_duration: Duration) -> bool {
        self.data.is_stale(stale_duration)
    }

    pub fn is_full(&self) -> bool {
        self.index.is_full()
    }

    pub fn base_offset(&self) -> u64 {
        self.base_offset
    }

    pub fn end_offset(&self) -> u64 {
        self.end_offset
    }
}

fn get_segment_paths(path: impl AsRef<Path>, base_offset: u64) -> (PathBuf, PathBuf) {
    let path = path.as_ref();
    let index_path = path.join(format!("{base_offset}.index"));
    let data_path = path.join(format!("{base_offset}.data"));
    (index_path, data_path)
}
