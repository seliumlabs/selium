mod list;

use crate::config::SharedLogConfig;
use crate::data::Data;
use crate::index::Index;
use crate::message::Message;
use anyhow::Result;
pub use list::SegmentList;
use std::cmp;
use std::ops::Range;
use std::path::{Path, PathBuf};

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
        let index = Index::open(index_path, config.clone()).await?;
        let data = Data::open(data_path, config).await?;
        let end_offset = index.current_offset() as u64;

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
        let index = Index::create(index_path, config.clone()).await?;
        let data = Data::create(data_path, config).await?;

        Ok(Self {
            index,
            data,
            base_offset,
            end_offset: 0,
        })
    }

    pub async fn read_slice(&self, offset_range: Range<u64>) -> Result<Vec<Message>> {
        let relative_offset = offset_range.start - self.base_offset;
        let relative_offset = cmp::max(relative_offset, 1);

        if let Some(start_entry) = self.index.lookup(relative_offset as u32)? {
            let end_offset = cmp::min(offset_range.end, self.end_offset);

            if let Some(end_entry) = self.index.lookup(end_offset as u32)? {
                let start_position = start_entry.physical_position();
                let end_position = end_entry.physical_position();
                let slice = self.data.read_slice(start_position, end_position).await?;
                return Ok(slice);
            }
        }

        Ok(vec![])
    }

    pub async fn write(&mut self, message: Message) -> Result<()> {
        let position = self.data.position();
        let timestamp = message.headers().timestamp();

        self.data.write(message).await?;
        self.index.append(timestamp, position).await?;
        self.end_offset += 1;

        Ok(())
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
