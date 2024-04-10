mod iterator;

use crate::error::Result;
use crate::message::Message;
pub use iterator::LogIterator;
use std::io::SeekFrom;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::fs;
use tokio::fs::{File, OpenOptions};
use tokio::io::{AsyncSeekExt, AsyncWriteExt, BufReader, BufWriter};

#[derive(Debug)]
pub struct Data {
    path: PathBuf,
    writer: BufWriter<File>,
    position: u64,
}

impl Data {
    pub async fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let file = OpenOptions::new().read(true).write(true).open(path).await?;
        let metadata = file.metadata().await?;
        let position = metadata.len();
        let writer = BufWriter::new(file);

        Ok(Self {
            path: path.to_owned(),
            writer,
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

        let writer = BufWriter::new(file);

        Ok(Self {
            path: path.to_owned(),
            writer,
            position: 0,
        })
    }

    pub async fn read_messages(
        &self,
        start_position: u64,
        end_position: Option<u64>,
    ) -> Result<LogIterator> {
        let end_position = end_position.unwrap_or(self.position);
        let file = File::open(&self.path).await?;
        let mut reader = BufReader::new(file);
        reader.seek(SeekFrom::Start(start_position)).await?;
        let log_slice = LogIterator::new(reader, end_position);

        Ok(log_slice)
    }

    pub async fn is_stale(&self, stale_duration: Duration) -> Result<bool> {
        let last_modified_time = self.writer.get_ref().metadata().await?.modified()?;

        let stale = last_modified_time
            .elapsed()
            .map_or(false, |elapsed| elapsed > stale_duration);

        Ok(stale)
    }

    pub async fn write(&mut self, message: Message) -> Result<()> {
        let length = message.headers().length();
        let mut buffer = Vec::with_capacity(message.headers().length() as usize);
        message.encode(&mut buffer);

        self.writer.write_all(&mut buffer).await?;
        self.position += length;

        Ok(())
    }

    pub async fn flush(&mut self) -> Result<()> {
        self.writer.flush().await?;
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
