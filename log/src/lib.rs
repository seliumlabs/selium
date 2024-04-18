mod index;
mod segment;
mod tasks;

pub mod config;
pub mod data;
pub mod error;
pub mod message;

use crate::{
    config::SharedLogConfig,
    error::{LogError, Result},
    message::{Message, MessageSlice},
    segment::SegmentList,
    tasks::{CleanerTask, FlusherTask},
};
use segment::SharedSegmentList;
use std::{ffi::OsStr, path::Path, sync::Arc};
use tokio::{
    fs,
    sync::{mpsc, RwLock},
};

#[derive(Debug)]
pub struct MessageLog {
    segments: SharedSegmentList,
    config: SharedLogConfig,
    flush_interrupt: mpsc::Sender<()>,
    _flusher: Arc<FlusherTask>,
    _cleaner: Arc<CleanerTask>,
}

impl MessageLog {
    pub async fn open(config: SharedLogConfig) -> Result<Self> {
        fs::create_dir_all(&config.segments_path)
            .await
            .map_err(LogError::CreateLogsDirectory)?;

        let segments = load_segments(config.clone()).await?;
        let (_flusher, flush_interrupt) = FlusherTask::start(config.clone(), segments.clone());
        let _cleaner = CleanerTask::start(config.clone(), segments.clone());

        Ok(Self {
            segments,
            config,
            flush_interrupt,
            _flusher,
            _cleaner,
        })
    }

    pub async fn write(&mut self, message: Message) -> Result<()> {
        self.segments.write().await.write(message).await?;
        self.try_flush().await?;
        Ok(())
    }

    pub async fn read_slice(&self, offset: u64, limit: Option<u64>) -> Result<MessageSlice> {
        self.segments.read().await.read_slice(offset, limit).await
    }

    pub async fn flush(&mut self) -> Result<()> {
        self.segments.write().await.flush().await?;
        let _ = self.flush_interrupt.send(()).await;
        Ok(())
    }

    pub async fn number_of_entries(&self) -> u64 {
        self.segments.read().await.number_of_entries()
    }

    async fn try_flush(&mut self) -> Result<()> {
        let segments = self.segments.read().await;

        let should_flush = match self.config.flush_policy.number_of_writes {
            Some(number_of_writes) => segments.writes_since_last_flush() >= number_of_writes,
            None => false,
        };

        drop(segments);

        if should_flush {
            self.flush().await?;
        }

        Ok(())
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

async fn load_segments(config: SharedLogConfig) -> Result<SharedSegmentList> {
    let path = &config.segments_path;
    let offsets = get_offsets(path).await?;

    let segments = if !offsets.is_empty() {
        SegmentList::from_offsets(&offsets, config.clone()).await?
    } else {
        SegmentList::create(config.clone()).await?
    };

    let shared_segments = Arc::new(RwLock::new(segments));
    Ok(shared_segments)
}
