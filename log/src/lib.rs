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
use std::{ffi::OsStr, path::Path, sync::Arc};
use tokio::{
    fs,
    sync::{broadcast, mpsc},
};

#[derive(Debug)]
pub struct MessageLog {
    segments: SegmentList,
    config: SharedLogConfig,
    number_of_entries: u64,
    writes_since_last_flush: u64,
    flush_interrupt: mpsc::Sender<()>,
    notifier: broadcast::Sender<()>,
    _flusher: Arc<FlusherTask>,
    _cleaner: Arc<CleanerTask>,
}

impl MessageLog {
    pub async fn open(config: SharedLogConfig) -> Result<Self> {
        fs::create_dir_all(&config.segments_path)
            .await
            .map_err(LogError::CreateLogsDirectory)?;

        let segments = load_segments(config.clone()).await?;
        let number_of_entries = segments.number_of_entries().await;
        let (tx, _) = broadcast::channel(1);
        let (_flusher, flush_interrupt) = FlusherTask::start(config.clone(), segments.clone());
        let _cleaner = CleanerTask::start(config.clone(), segments.clone());

        Ok(Self {
            segments,
            config,
            number_of_entries,
            writes_since_last_flush: 0,
            flush_interrupt,
            notifier: tx,
            _flusher,
            _cleaner,
        })
    }

    pub async fn write(&mut self, message: Message) -> Result<()> {
        self.segments.write(message).await?;
        self.writes_since_last_flush += 1;
        self.try_flush().await?;
        Ok(())
    }

    pub async fn read_slice(&self, offset: u64, limit: Option<u64>) -> Result<MessageSlice> {
        if offset > self.number_of_entries {
            Ok(MessageSlice::empty(offset))
        } else {
            self.segments.read_slice(offset, limit).await
        }
    }

    pub async fn flush(&mut self) -> Result<()> {
        self.segments.flush().await?;
        self.number_of_entries += self.writes_since_last_flush;
        let _ = self.flush_interrupt.send(()).await;
        Ok(())
    }

    pub fn number_of_entries(&self) -> u64 {
        self.number_of_entries
    }

    pub fn add_listener(&mut self) -> broadcast::Receiver<()> {
        self.notifier.subscribe()
    }

    async fn try_flush(&mut self) -> Result<()> {
        let should_flush = match self.config.flush_policy.number_of_writes {
            Some(number_of_writes) => self.writes_since_last_flush >= number_of_writes,
            None => false,
        };

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

async fn load_segments(config: SharedLogConfig) -> Result<SegmentList> {
    let path = &config.segments_path;
    let offsets = get_offsets(path).await?;

    let segments = if !offsets.is_empty() {
        SegmentList::from_offsets(&offsets, config.clone()).await?
    } else {
        SegmentList::create(config.clone()).await?
    };

    Ok(segments)
}
