//! Contains the [Index] and [Mmap] types.

mod entry;
mod mmap;

use crate::{config::SharedLogConfig, error::Result};
pub use entry::IndexEntry;
pub use mmap::Mmap;
use std::path::Path;

/// Wrapper type for an index file belonging to a segment.
///
/// The index is a memory-mapped file that serves as a lookup directory of all messages in the segment.
///
/// The index lists where any messages in the log can be located via byte offset. Given a relative offset,
/// a fast lookup of a byte offset in the data file can be performed via a binary search.
#[derive(Debug)]
pub struct Index {
    mmap: Mmap,
    current_offset: u32,
    config: SharedLogConfig,
}

impl Index {
    /// Constructs a new Index instance.
    pub fn new(mmap: Mmap, current_offset: u32, config: SharedLogConfig) -> Self {
        Self {
            mmap,
            current_offset,
            config,
        }
    }

    /// Constructs an Index instance from an existing index file.
    ///
    /// # Errors
    /// - Returns Err if a failure occurs while opening the existing index file.
    pub async fn open(path: impl AsRef<Path>, config: SharedLogConfig) -> Result<Self> {
        let mmap = Mmap::load(path).await?;
        let next_offset = mmap.get_current_offset();
        Ok(Self::new(mmap, next_offset, config))
    }

    /// Constructs an Index instance, and creates the underlying memory-mapped index file.
    ///
    /// # Errors
    /// - Returns Err if a failure occurs while creating the index file.
    pub async fn create(path: impl AsRef<Path>, config: SharedLogConfig) -> Result<Self> {
        let mmap = Mmap::create(path, config.clone()).await?;
        Ok(Self::new(mmap, 0, config))
    }

    /// Appends a new entry to the memory-mapped index.
    /// This method is called after appending an encoded message to the segment's data file.
    ///
    /// # Params
    /// * `timestamp` - The UNIX timestamp corresponding to when the message was appended the data file.
    /// * `file_position` - The byte offset in the data file for the appended message.
    pub fn append(&mut self, timestamp: u64, file_position: u64) {
        if self.current_offset <= self.config.max_index_entries {
            let next_offset = self.current_offset + 1;
            let entry = IndexEntry::new(next_offset, timestamp, file_position);
            self.mmap.push(entry);
            self.current_offset = next_offset;
        }
    }

    /// Flushes the memory map to the underlying file.
    /// This operation can have negative performance impacts, so should be used sparingly until
    /// replication has been implemented.
    ///
    /// # Errors
    /// - Returns Err if the memory map fails to flush to the underlying file.
    pub fn flush(&mut self) -> Result<()> {
        self.mmap.flush()?;
        Ok(())
    }

    /// Performs a lookup in the memory map for the specified `relative_offset`, and returns
    /// the decoded [IndexEntry].
    ///
    /// Returns [Option::None] if the relative offset does not exist in the index.
    pub fn lookup(&self, relative_offset: u32) -> Option<IndexEntry> {
        if relative_offset > self.current_offset {
            return None;
        }

        self.mmap
            .find(|entry| entry.relative_offset() == relative_offset)
    }

    /// Removes the underlying index file.
    /// This method is typically only called when a segment is being removed by the log cleaner task.
    ///
    /// # Errors
    /// - Returns Err if the underlying file cannot be removed.
    pub async fn remove(self) -> Result<()> {
        self.mmap.remove().await?;
        Ok(())
    }

    /// The current relative offset in the index.
    pub fn current_offset(&self) -> u32 {
        self.current_offset
    }

    /// Returns true if the index is at capacity, based on the provided `max_index_entries` option
    /// in the shared log configuration.
    pub fn is_full(&self) -> bool {
        self.current_offset == self.config.max_index_entries
    }
}
