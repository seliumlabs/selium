use crate::message::{LogCodec, Message};
use anyhow::Result;
use futures::{SinkExt, StreamExt};
use std::time::{Duration, SystemTime};
use std::{
    io::SeekFrom,
    path::{Path, PathBuf},
};
use tokio::fs;
use tokio::io::{AsyncReadExt, Take};
use tokio::{
    fs::{File, OpenOptions},
    io::{AsyncSeekExt, BufWriter},
};
use tokio_util::codec::{FramedRead, FramedWrite};

#[derive(Debug)]
pub struct Data {
    path: PathBuf,
    position: u64,
    writer: FramedWrite<BufWriter<File>, LogCodec>,
    last_modified_time: SystemTime,
}

impl Data {
    pub async fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let file = OpenOptions::new().read(true).write(true).open(path).await?;
        let metadata = file.metadata().await?;
        let last_modified_time = metadata.modified()?;

        let mut writer = BufWriter::new(file);
        writer.seek(SeekFrom::End(0)).await?;
        let position = writer.stream_position().await?;
        let writer = FramedWrite::new(writer, LogCodec);

        Ok(Self {
            path: path.to_owned(),
            writer,
            position,
            last_modified_time,
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

        let metadata = file.metadata().await?;
        let last_modified_time = metadata.modified()?;

        let writer = BufWriter::new(file);
        let writer = FramedWrite::new(writer, LogCodec);

        Ok(Self {
            path: path.to_owned(),
            writer,
            position: 0,
            last_modified_time,
        })
    }

    pub async fn read_messages(
        &self,
        start_position: u64,
        end_position: u64,
    ) -> Result<Vec<Message>> {
        let mut reader = new_reader(&self.path, start_position, end_position).await?;
        let mut messages = vec![];

        while let Some(Ok(message)) = reader.next().await {
            messages.push(message)
        }

        Ok(messages)
    }

    pub fn is_stale(&self, stale_duration: Duration) -> bool {
        self.last_modified_time
            .elapsed()
            .map_or(false, |elapsed| elapsed > stale_duration)
    }

    pub async fn write(&mut self, message: Message) -> Result<()> {
        let length = message.headers().length();
        self.writer.send(message).await?;
        self.writer.flush().await?;
        self.position += length;

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

async fn new_reader(
    path: impl AsRef<Path>,
    start_position: u64,
    end_position: u64,
) -> Result<FramedRead<Take<File>, LogCodec>> {
    let slice_length = (end_position - start_position) as usize;
    let mut file = File::open(path).await?;
    file.seek(SeekFrom::Start(start_position)).await?;
    let slice = file.take(slice_length as u64);

    Ok(FramedRead::new(slice, LogCodec))
}
