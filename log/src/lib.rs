//! A library containing an implementation of an ordered, log-based message queue.
//!
//! Selium Log aims to provide as simple an abstraction as possible over the message log,
//! in order to make it easy to provision and use in your libraries.
//!
//! The message log's structure should be familiar to those coming from an Apache Kafka background: a log
//! is represented by one or more segments, with each segment comprised of a memory-mapped index file, serving as
//! a lookup for the segment's records, and an append-only data file containing said records.
//!
//! Only the most current segment can be designated as the mutable ("hot") segment, while the older segments
//! are read-only until their eventual cleanup. Once the current hot segment exceeds the defined
//! [LogConfig::max_index_entries](crate::config::LogConfig::max_index_entries) threshold, it will also become
//! read-only, while a new segment is created and assigned as the hot segment.
//!
//! Replication has not yet been implemented for Selium Log as of this release, but is a planned feature.
//! Due to this, durability can be tough to achieve alongside high throughput on a single node. Most of the
//! latency comes from the I/O overhead of flushing the memory-mapped index and data files to the filesystem.
//! To compensate for this overhead, the flushing frequency can be tweaked via the
//! [FlushPolicy](crate::config::FlushPolicy) struct, to strike a balance between durability and throughput.

mod tasks;

pub mod config;
pub mod data;
pub mod error;
pub mod index;
pub mod message;
pub mod segment;

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

/// The entry point to Selium Log.
///
/// The MessageLog struct manages creating and opening logs, along with provisioning the
/// [SegmentList], and coordinates atomic reads and writes.
///
/// The MessageLog also creates the Flusher and Cleaner asynchronous tasks, and takes ownership of them to
/// assure that the tasks are gracefully terminated when the MessageLog instance is destroyed.
///
/// # Examples
/// ```
/// use anyhow::Result;
/// use selium_log::{config::LogConfig, message::Message, MessageLog};
/// use std::sync::Arc;
///
/// const MESSAGE_VERSION: u32 = 1;
///
/// #[tokio::main]
/// async fn main() -> Result<()> {
///     let config = LogConfig::from_path("path/to/segments/dir");
///     let log = MessageLog::open(Arc::new(config)).await?;
///     let message = Message::single(b"Hello, world!", MESSAGE_VERSION);
///
///     log.write(message).await?;
///     log.flush().await?;
///     let slice = log.read_slice(0, None).await?;
///
///     if let Some(mut iter) = slice.messages() {
///         let next = iter.next().await?;
///         println!("{next:?}")
///     }

///     Ok(())
/// }
/// ```
#[derive(Debug)]
pub struct MessageLog {
    segments: SharedSegmentList,
    config: SharedLogConfig,
    flush_interrupt: mpsc::Sender<()>,
    _flusher: Arc<FlusherTask>,
    _cleaner: Arc<CleanerTask>,
}

impl MessageLog {
    /// Opens a message log at the segments directory configured in the provided `config`
    /// argument.
    ///
    /// If the log directory does not yet exist, it will be created.
    ///
    /// Existing segments will be loaded from the filesystem, and provisioned as a
    /// [SegmentList] instance. If no segments exist in the log directory,
    /// a single "hot" segment will be created.
    ///
    /// The Flusher and Cleaner asynchronous tasks will also be started, and will run in the background.
    ///
    /// # Errors
    /// - Returns [LogError::CreateLogsDirectory] if an error occurs while creating the log directory.
    /// - Returns Err if an error occurs while constructing the [SegmentList].
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

    /// Writes the provided [Message] to the current hot segment.
    ///
    /// If the hot segment is at full capacity following the write, the current hot segment
    /// will be flushed, and a new segment will be created and designated as the hot segment in
    /// its place. Otherwise, the `writes_since_last_flush` field is incremented by 1.
    ///
    /// # Errors
    /// - Returns [LogError::SegmentListEmpty] if there are no segments in the list yet.
    /// - Returns Err if writing to the hot segment fails.
    /// - Returns Err if the segment is full, and the current hot segment fails to flush.
    /// - Returns Err if the segment is full, and the new hot segment fails to be created.
    pub async fn write(&self, message: Message) -> Result<()> {
        self.segments.write().await.write(message).await?;
        self.try_flush().await?;
        Ok(())
    }

    /// Reads a range of messages from a segment identified by the provided offset.
    ///
    /// Returns an empty [MessageSlice] if the provided offset is greater than the total
    /// amount of entries in the log.
    ///
    /// # Params
    /// * `offset` - The starting offset, used to locate the segment and relative offset.
    /// * `limit` - An optional message read limit.
    pub async fn read_slice(&self, offset: u64, limit: Option<u64>) -> Result<MessageSlice> {
        self.segments.read().await.read_slice(offset, limit).await
    }

    /// Flushes the hot segment to the filesystem.
    /// The Flusher task interval will also be interrupted and reset.
    ///
    /// # Errors
    /// - Returns Err if the hot segment fails to flush.
    pub async fn flush(&self) -> Result<()> {
        self.segments.write().await.flush().await?;
        let _ = self.flush_interrupt.send(()).await;
        Ok(())
    }

    /// Retrieves the total number of entries in the log, based on the `end_offset` in the current
    /// hot segment.
    pub async fn number_of_entries(&self) -> u64 {
        self.segments.read().await.number_of_entries()
    }

    async fn try_flush(&self) -> Result<()> {
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
