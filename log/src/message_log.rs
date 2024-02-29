use crate::{config::SharedLogConfig, message::Message, segment::SegmentList};
use anyhow::Result;
use std::{
    ffi::OsStr,
    ops::Range,
    path::{Path, PathBuf},
};
use tokio::fs;

#[derive(Debug)]
pub struct MessageLog {
    segments: SegmentList,
    config: SharedLogConfig,
}

impl MessageLog {
    pub async fn open(config: SharedLogConfig) -> Result<Self> {
        let path = config.segments_path();
        fs::create_dir_all(path).await?;
        let segments = load_segments(config.clone()).await?;

        Ok(Self { segments, config })
    }

    pub async fn write(&mut self, message: Message) -> Result<()> {
        self.segments.write(message).await?;
        Ok(())
    }

    pub async fn read_messages(&self, offset_range: Range<u64>) -> Result<Vec<Message>> {
        let messages = self.segments.read_slice(offset_range).await?;
        Ok(messages)
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

async fn load_segments(config: SharedLogConfig) -> Result<SegmentList> {
    let path = config.segments_path();
    let offsets = get_offsets(path).await?;

    let segments = if !offsets.is_empty() {
        SegmentList::from_offsets(&offsets, config.clone()).await?
    } else {
        SegmentList::create(config.clone()).await?
    };

    Ok(segments)
}
