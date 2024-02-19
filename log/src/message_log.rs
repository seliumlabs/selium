use crate::{
    config::SharedLogConfig,
    data::Message,
    segment::{list::SegmentList, MutSegment},
};
use anyhow::Result;
use bytes::Bytes;
use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
};
use tokio::fs;

#[derive(Debug)]
pub struct MessageLog {
    path: PathBuf,
    segments: SegmentList,
    hot_segment: MutSegment,
}

impl MessageLog {
    pub async fn create(path: impl AsRef<Path>, config: SharedLogConfig) -> Result<Self> {
        let path = path.as_ref();
        fs::create_dir_all(path).await?;

        let segments = SegmentList::default();
        let hot_segment = MutSegment::create(path, 0, config).await?;

        Ok(Self {
            path: path.to_owned(),
            segments,
            hot_segment,
        })
    }

    pub async fn open(path: impl AsRef<Path>, config: SharedLogConfig) -> Result<Self> {
        let path = path.as_ref();
        let (segments, hot_segment) = load_segments(path, config.clone()).await?;

        Ok(Self {
            path: path.to_owned(),
            segments,
            hot_segment,
        })
    }

    pub async fn write(&mut self, records: Vec<Bytes>) -> Result<()> {
        let timestamp = chrono::Utc::now().timestamp() as u64;
        let message = Message::new(timestamp, &records);
        self.hot_segment.write(message).await?;
        Ok(())
    }
}

fn is_index_file(path: &PathBuf) -> bool {
    path.is_file() && path.extension() == Some("index".as_ref())
}

async fn get_offsets(path: impl AsRef<Path>) -> Result<Vec<u64>> {
    let mut offsets = vec![];

    while let Some(entry) = fs::read_dir(&path).await?.next_entry().await? {
        let maybe_offset = entry
            .path()
            .file_name()
            .and_then(OsStr::to_str)
            .map(|s| s.trim_end_matches(".index"))
            .and_then(|s| s.parse().ok());

        if let Some(offset) = maybe_offset {
            offsets.push(offset);
        }
    }

    Ok(offsets)
}

async fn load_segments(
    path: impl AsRef<Path>,
    config: SharedLogConfig,
) -> Result<(SegmentList, MutSegment)> {
    let path = path.as_ref();
    let mut offsets = get_offsets(path).await?;

    if !offsets.is_empty() {
        let last_offset = offsets.pop().unwrap();
        let segments = SegmentList::from_offsets(path, &offsets, config.clone()).await?;
        let hot_segment = MutSegment::open(path, last_offset, config).await?;
        Ok((segments, hot_segment))
    } else {
        let segments = SegmentList::default();
        let hot_segment = MutSegment::create(path, 0, config).await?;
        Ok((segments, hot_segment))
    }
}
