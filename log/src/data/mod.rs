//! Contains the [Data] type, along with a [LogIterator] to iterate over the data file.

mod iterator;

use crate::error::Result;
use crate::message::Message;
use bytes::BytesMut;
pub use iterator::LogIterator;
use std::io::SeekFrom;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::fs;
use tokio::fs::{File, OpenOptions};
use tokio::io::{AsyncSeekExt, AsyncWriteExt, BufReader};

/// Wrapper type for a data file belonging to a log segment.
///
/// The data file is quite simple: the messages are encoded into a particular format, before being
/// stored sequentially in this file. The data file is seldom accessed without first consulting the
/// index file for an initial byte offset.
#[derive(Debug)]
pub struct Data {
    path: PathBuf,
    file: File,
    buffer: BytesMut,
    position: u64,
}

impl Data {
    /// Creates a new Data instance by opening an existing data file.
    ///
    /// # Errors
    /// - Returns Err if the existing data file cannot be opened.
    pub async fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let file = OpenOptions::new().read(true).write(true).open(path).await?;
        let metadata = file.metadata().await?;
        let position = metadata.len();

        Ok(Self {
            path: path.to_owned(),
            file,
            buffer: BytesMut::new(),
            position,
        })
    }

    /// Creates a new Data instance, along with the underlying data file.
    ///
    /// # Errors
    /// - Returns Err if the underlying file cannot be created.
    pub async fn create(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();

        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .read(true)
            .mode(0o600)
            .open(path)
            .await?;

        Ok(Self {
            path: path.to_owned(),
            file,
            buffer: BytesMut::new(),
            position: 0,
        })
    }

    /// Creates an iterator over a range of message in the data file.
    ///
    /// Opens a new file handle at the data file path, and wraps it in a buffered reader
    /// to iterate over.
    ///
    /// # Params
    /// * `start_position` - The starting byte offset in the data file.
    /// * `end_position`   - An optional end_position to limit the amount of messages to read.
    ///                      If not provided, messages will be read from the entire segment, beginning
    ///                      from `start_position`.
    ///
    /// # Errors
    /// - Returns Err if the file fails to be opened.
    /// - Returns Err if the buffered reader cannot seek to the `start_position`.
    pub async fn read_messages(
        &self,
        start_position: u64,
        end_position: Option<u64>,
    ) -> Result<LogIterator> {
        let file = File::open(&self.path).await?;
        let mut reader = BufReader::new(file);
        reader.seek(SeekFrom::Start(start_position)).await?;

        let end_position = end_position.unwrap_or(self.position);
        let log_slice = LogIterator::new(reader, start_position, end_position);

        Ok(log_slice)
    }

    /// Returns true if the data file's last modified time falls outside of the
    /// provided `stale_duration`.
    ///
    /// This method is used to determine which segments can be cleaned by the cleaner task.
    ///
    /// # Errors
    /// - Returns Err if the data file's metadata cannot be accessed.
    pub async fn is_stale(&self, stale_duration: Duration) -> Result<bool> {
        let last_modified_time = self.file.metadata().await?.modified()?;

        let stale = last_modified_time
            .elapsed()
            .map_or(false, |elapsed| elapsed > stale_duration);

        Ok(stale)
    }

    /// Encodes the provided [Message], and writes it to the write buffer, advancing the
    /// current position.
    pub async fn write(&mut self, message: Message) {
        let length = message.headers().length();
        let mut buffer = Vec::with_capacity(length as usize);
        message.encode(&mut buffer);

        self.buffer.extend_from_slice(&buffer);
        self.position += length;
    }

    /// Flushes the write buffer to the data file.
    ///
    /// # Errors
    /// - Returns Err if writing the buffer to the data file fails.
    /// - Returns Err if flushing the data file fails.
    pub async fn flush(&mut self) -> Result<()> {
        let buffer = std::mem::replace(&mut self.buffer, BytesMut::new());
        self.file.write_all(&buffer).await?;
        self.file.flush().await?;
        Ok(())
    }

    /// Removes the data file from the filesystem.
    ///
    /// This method is typically only called by the log cleaner task to remove data files
    /// belonging to stale segments.
    ///
    /// # Errors
    /// - Returns Err if the data file fails to be removed.
    pub async fn remove(self) -> Result<()> {
        fs::remove_file(&self.path).await?;
        Ok(())
    }

    /// The current byte position in the data file.
    pub fn position(&self) -> u64 {
        self.position
    }
}
