mod list;

use crate::config::SharedLogConfig;
use crate::data::{Data, MutData};
use crate::index::{Index, MutIndex};
use crate::message::Message;
use crate::traits::SegmentCommon;
use anyhow::Result;
use async_trait::async_trait;
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
        let end_offset = index.next_offset() as u64;

        Ok(Self {
            index,
            data,
            base_offset,
            end_offset,
        })
    }

    pub fn base_offset(&self) -> u64 {
        self.base_offset
    }

    pub fn end_offset(&self) -> u64 {
        self.end_offset
    }
}

#[async_trait]
impl SegmentCommon for Segment {
    async fn read_slice(&self, offset_range: Range<u64>) -> Result<Vec<Message>> {
        read_slice(
            self.base_offset,
            self.end_offset,
            offset_range,
            &self.index,
            &self.data,
        )
        .await
    }
}

#[derive(Debug)]
pub struct MutSegment {
    index: MutIndex,
    data: MutData,
    base_offset: u64,
    end_offset: u64,
}

impl MutSegment {
    pub async fn open(base_offset: u64, config: SharedLogConfig) -> Result<Self> {
        let path = config.segments_path();
        let (index_path, data_path) = get_segment_paths(path, base_offset);
        let index = MutIndex::open(index_path, config.clone()).await?;
        let data = MutData::open(data_path, config).await?;
        let end_offset = index.next_offset() as u64 - 1;

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
        let index = MutIndex::create(index_path, config.clone()).await?;
        let data = MutData::create(data_path, config).await?;

        Ok(Self {
            index,
            data,
            base_offset,
            end_offset: 0,
        })
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
}

#[async_trait]
impl SegmentCommon for MutSegment {
    async fn read_slice(&self, offset_range: Range<u64>) -> Result<Vec<Message>> {
        read_slice(
            self.base_offset,
            self.end_offset,
            offset_range,
            &self.index,
            &self.data,
        )
        .await
    }
}

fn get_segment_paths(path: impl AsRef<Path>, base_offset: u64) -> (PathBuf, PathBuf) {
    let path = path.as_ref();
    let index_path = path.join(format!("{base_offset}.index"));
    let data_path = path.join(format!("{base_offset}.data"));
    (index_path, data_path)
}

async fn read_slice<I, D>(
    base_offset: u64,
    end_offset: u64,
    offset_range: Range<u64>,
    index: &I,
    data: &D,
) -> Result<Vec<Message>>
where
    I: IndexCommon,
    D: DataCommon,
{
    let relative_offset = offset_range.start - base_offset;

    if let Some(start_entry) = index.lookup(relative_offset as u32)? {
        let end_offset = cmp::min(offset_range.end, end_offset);

        if let Some(end_entry) = index.lookup(end_offset as u32)? {
            let start_position = start_entry.physical_position();
            let end_position = end_entry.physical_position();
            let slice = data.read_slice(start_position, end_position).await?;
            return Ok(slice);
        }
    }

    Ok(vec![])
}
