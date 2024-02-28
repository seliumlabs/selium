use crate::{
    config::SharedLogConfig,
    message::Message,
    segment::{MutSegment, SegmentList},
    traits::SegmentCommon,
};
use anyhow::Result;
use std::{
    ffi::OsStr,
    ops::Range,
    path::{Path, PathBuf},
};
use tokio::fs;

#[derive(Debug)]
pub struct ReadOptions {
    offset: u64,
    count: u64,
}

impl ReadOptions {
    pub fn new(offset: u64, count: u64) -> Self {
        Self { offset, count }
    }

    pub fn offset(&self) -> u64 {
        self.offset
    }

    pub fn count(&self) -> u64 {
        self.count
    }
}

#[derive(Debug)]
pub struct MessageLog {
    segments: SegmentList,
    hot_segment: MutSegment,
    config: SharedLogConfig,
}

impl MessageLog {
    pub async fn open(config: SharedLogConfig) -> Result<Self> {
        let path = config.segments_path();
        fs::create_dir_all(path).await?;
        let (segments, hot_segment) = load_segments(config.clone()).await?;

        Ok(Self {
            segments,
            hot_segment,
            config,
        })
    }

    pub async fn write(&mut self, message: Message) -> Result<()> {
        self.hot_segment.write(message).await?;
        Ok(())
    }

    pub async fn read_messages(&self, offset_range: Range<u64>) -> Result<Vec<Message>> {
        let hot_base_offset = self.hot_segment.base_offset();

        if offset_range.start >= hot_base_offset {
            self.hot_segment.read_slice(offset_range).await
        } else {
            self.segments.read_slice(offset_range).await
        }
    }
}

fn is_index_file(path: &PathBuf) -> bool {
    path.is_file() && path.extension() == Some("index".as_ref())
}

async fn get_offsets(path: impl AsRef<Path>) -> Result<Vec<u64>> {
    let mut offsets = vec![];
    let mut entries = fs::read_dir(&path).await?;

    while let Some(entry) = entries.next_entry().await? {
        let path = entry.path();

        if is_index_file(&path) {
            if let Some(offset) = path
                .file_name()
                .and_then(OsStr::to_str)
                .map(|s| s.trim_end_matches(".index"))
                .and_then(|s| s.parse().ok())
            {
                offsets.push(offset);
            }
        }
    }

    Ok(offsets)
}

async fn load_segments(config: SharedLogConfig) -> Result<(SegmentList, MutSegment)> {
    let path = config.segments_path();
    let mut offsets = get_offsets(path).await?;

    if !offsets.is_empty() {
        let last_offset = offsets.pop().unwrap();
        let segments = SegmentList::from_offsets(&offsets, config.clone()).await?;
        let hot_segment = MutSegment::open(last_offset, config.clone()).await?;
        Ok((segments, hot_segment))
    } else {
        let segments = SegmentList::default();
        let hot_segment = MutSegment::create(0, config.clone()).await?;
        Ok((segments, hot_segment))
    }
}
