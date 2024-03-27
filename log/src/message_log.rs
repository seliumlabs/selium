use crate::{
    config::SharedLogConfig,
    error::{LogError, Result},
    log_cleaner::LogCleaner,
    message::{Message, MessageSlice},
    segment::SegmentList,
};
use std::{ffi::OsStr, ops::Range, path::Path, sync::Arc};
use tokio::fs;

#[derive(Debug)]
pub struct MessageLog {
    segments: SegmentList,
    _cleaner: Arc<LogCleaner>,
}

impl MessageLog {
    pub async fn open(config: SharedLogConfig) -> Result<Self> {
        fs::create_dir_all(config.segments_path())
            .await
            .map_err(LogError::CreateLogsDirectory)?;

        let segments = load_segments(config.clone()).await?;
        let _cleaner = LogCleaner::start(config, segments.clone());

        Ok(Self { segments, _cleaner })
    }
    pub async fn write(&mut self, message: Message) -> Result<()> {
        self.segments.write(message).await?;
        Ok(())
    }

    pub async fn read_slice(&self, offset_range: Range<u64>) -> Result<MessageSlice> {
        let slice = self.segments.read_slice(offset_range).await?;
        Ok(slice)
    }
}

fn is_index_file(path: &Path) -> bool {
    path.is_file() && path.extension() == Some("index".as_ref())
}

async fn get_offsets(path: impl AsRef<Path>) -> Result<Vec<u64>> {
    let mut offsets = vec![];
    let mut entries = fs::read_dir(&path).await.map_err(LogError::LoadSegments)?;

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
