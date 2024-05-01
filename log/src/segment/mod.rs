//! Contains the [Segment] and [SegmentList] types.

mod list;

use crate::config::SharedLogConfig;
use crate::data::Data;
use crate::error::Result;
use crate::index::Index;
use crate::message::{Message, MessageSlice};
pub use list::{SegmentList, SharedSegmentList};
use std::cmp;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// An individual hot or cold segment for a log.
///
/// A segment is comprised of two components: a memory-mapped index file, serving as a lookup for
/// the segment's records, and an append-only data file containing the encoded records.
///
/// When a segment is created, two files will be created in the directory pertaining to that log,
/// which will represent the index and data files. The files will use the naming convention of
/// \<base-offset\>.index and \<base-offset\>.data, respectively.
#[derive(Debug)]
pub struct Segment {
    index: Index,
    data: Data,
    base_offset: u64,
    end_offset: u64,
}

impl Segment {
    /// Constructs a Segment instance, opening the index/data file pair for the corresponding
    /// `base_offset`.
    ///
    /// # Errors
    /// - Returns Err if an error occurs while loading the Index portion.
    /// - Returns Err if an error occurs while loading the Data portion.
    pub async fn open(base_offset: u64, config: SharedLogConfig) -> Result<Self> {
        let path = &config.segments_path;
        let (index_path, data_path) = get_segment_paths(path, base_offset);
        let index = Index::open(index_path, config).await?;
        let data = Data::open(data_path).await?;
        let end_offset = base_offset + index.current_offset() as u64;

        Ok(Self {
            index,
            data,
            base_offset,
            end_offset,
        })
    }

    /// Constructs a Segment instance, creating the accompanying index/data file pair for the
    /// corresponding `base_offset`.
    ///
    /// # Errors
    /// - Returns Err if an error occurs while creating the Index portion.
    /// - Returns Err if an error occurs while creating the Data portion.
    pub async fn create(base_offset: u64, config: SharedLogConfig) -> Result<Self> {
        let path = &config.segments_path;
        let (index_path, data_path) = get_segment_paths(path, base_offset);
        let index = Index::create(index_path, config).await?;
        let data = Data::create(data_path).await?;

        Ok(Self {
            index,
            data,
            base_offset,
            end_offset: base_offset,
        })
    }

    /// Reads a range of messages from this segment, starting from the provided offset.
    ///
    /// Returns an empty [MessageSlice] if the provided offset is greater than the total amount of
    /// entries in the log.
    ///
    /// # Params
    /// * `offset` - The starting offset, used to locate the relative offset in this segment.
    /// * `limit` - An optional message read limit.
    ///
    /// # Errors
    /// - Returns Err if an error occurs while reading from the data file.
    pub async fn read_slice(&self, offset: u64, limit: Option<u64>) -> Result<MessageSlice> {
        let end_offset = limit.map_or(self.end_offset, |e| {
            cmp::min(offset + e + 1, self.end_offset)
        });
        let relative_start_offset = offset - self.base_offset + 1;
        let relative_end_offset = end_offset - self.base_offset;

        if let Some(start_entry) = self.index.lookup(relative_start_offset as u32) {
            let start_pos = start_entry.physical_position();

            if end_offset == self.end_offset {
                let messages = self.data.read_messages(start_pos, None).await?;
                return Ok(MessageSlice::new(messages, end_offset + 1));
            }

            if let Some(end_entry) = self.index.lookup(relative_end_offset as u32) {
                let end_pos = end_entry.physical_position();
                let messages = self.data.read_messages(start_pos, Some(end_pos)).await?;
                return Ok(MessageSlice::new(messages, end_offset));
            }
        }

        Ok(MessageSlice::empty(offset))
    }

    /// Writes the provided [Message] to the write buffer, and then appends a new
    /// [IndexEntry](crate::index::IndexEntry) to the index memory-map.
    pub async fn write(&mut self, message: Message) {
        let position = self.data.position();
        let timestamp = message.headers().timestamp();

        self.data.write(message).await;
        self.index.append(timestamp, position);
        self.end_offset += 1;
    }

    /// Flushes the write buffer to the data file and the index memory-map to the filesystem.
    ///
    /// # Errors
    /// - Returns Err if an error occurs while flushing either file.
    pub async fn flush(&mut self) -> Result<()> {
        self.index.flush()?;
        self.data.flush().await?;
        Ok(())
    }

    /// Removes the data file and index files from the filesystem.
    /// This method is typically called by the Log Cleaner task.
    ///
    /// # Errors
    /// - Returns Err if an errors occurs during the call to remove either the index or data file.
    pub async fn remove(self) -> Result<()> {
        self.data.remove().await?;
        self.index.remove().await?;
        Ok(())
    }

    /// Returns true if the data file's last modified time falls outside of the
    /// provided `stale_duration`.
    ///
    /// This method is used to determine which segments can be cleaned by the cleaner task.
    ///
    /// # Errors
    /// - Returns Err if the data file's metadata cannot be accessed.
    pub async fn is_stale(&self, stale_duration: Duration) -> Result<bool> {
        self.data.is_stale(stale_duration).await
    }

    /// Returns true if the index is at capacity, based on the provided `max_index_entries` option
    /// in the shared log configuration.
    pub fn is_full(&self) -> bool {
        self.index.is_full()
    }

    /// The next offset to write in this segment, or the maximum offset if the segment is at
    /// full capacity.
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
