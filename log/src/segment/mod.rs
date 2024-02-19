pub mod list;

use crate::config::SharedLogConfig;
use crate::data::{Data, Message, MutData};
use crate::index::{Index, MutIndex};
use anyhow::Result;
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub struct Segment {
    index: Index,
    data: Data,
    base_offset: u64,
    end_offset: u64,
    created_timestamp: u64,
}

impl Segment {
    pub async fn open(
        path: impl AsRef<Path>,
        base_offset: u64,
        config: SharedLogConfig,
    ) -> Result<Self> {
        let (index_path, data_path) = get_segment_paths(path, base_offset);
        let index = Index::open(index_path, config.clone()).await?;
        let data = Data::open(data_path, config).await?;

        Ok(Self {
            index,
            data,
            base_offset,
            end_offset: 0,
            created_timestamp: 0,
        })
    }

    pub fn base_offset(&self) -> u64 {
        self.base_offset
    }

    pub fn end_offset(&self) -> u64 {
        self.end_offset
    }

    pub fn created_timestamp(&self) -> u64 {
        self.created_timestamp
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
    pub async fn open(
        path: impl AsRef<Path>,
        base_offset: u64,
        config: SharedLogConfig,
    ) -> Result<Self> {
        let (index_path, data_path) = get_segment_paths(path, base_offset);
        let index = MutIndex::open(index_path, config.clone()).await?;
        let data = MutData::open(data_path, config).await?;
        let end_offset = index.next_offset() as u64;

        Ok(Self {
            index,
            data,
            base_offset,
            end_offset,
        })
    }

    pub async fn create(
        path: impl AsRef<Path>,
        base_offset: u64,
        config: SharedLogConfig,
    ) -> Result<Self> {
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
        let timestamp = message.timestamp();

        self.data.write(message).await?;
        self.index.append(timestamp, position).await?;
        self.end_offset += 1;

        Ok(())
    }
}

pub fn get_segment_paths(path: impl AsRef<Path>, base_offset: u64) -> (PathBuf, PathBuf) {
    let path = path.as_ref();
    let index_path = path.join(format!("{base_offset}.index"));
    let data_path = path.join(format!("{base_offset}.data"));
    (index_path, data_path)
}
