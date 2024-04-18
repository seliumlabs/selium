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

#[derive(Debug)]
pub struct Data {
    path: PathBuf,
    file: File,
    buffer: BytesMut,
    position: u64,
}

impl Data {
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

    pub async fn create(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();

        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .read(true)
            .open(path)
            .await?;

        Ok(Self {
            path: path.to_owned(),
            file,
            buffer: BytesMut::new(),
            position: 0,
        })
    }

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

    pub async fn is_stale(&self, stale_duration: Duration) -> Result<bool> {
        let last_modified_time = self.file.metadata().await?.modified()?;

        let stale = last_modified_time
            .elapsed()
            .map_or(false, |elapsed| elapsed > stale_duration);

        Ok(stale)
    }

    pub async fn write(&mut self, message: Message) -> Result<()> {
        let length = message.headers().length();
        let mut buffer = Vec::with_capacity(length as usize);
        message.encode(&mut buffer);

        self.buffer.extend_from_slice(&buffer);
        self.position += length;

        Ok(())
    }

    pub async fn flush(&mut self) -> Result<()> {
        let buffer = std::mem::replace(&mut self.buffer, BytesMut::new());
        self.file.write_all(&buffer).await?;
        Ok(())
    }

    pub async fn remove(self) -> Result<()> {
        fs::remove_file(&self.path).await?;
        Ok(())
    }

    pub fn position(&self) -> u64 {
        self.position
    }
}
