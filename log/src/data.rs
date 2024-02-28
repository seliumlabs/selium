use crate::config::SharedLogConfig;
use crate::message::{LogCodec, Message};
use anyhow::Result;
use futures::{SinkExt, Stream, StreamExt, TryStreamExt};
use std::{
    io::SeekFrom,
    path::{Path, PathBuf},
};
use tokio::io::{AsyncRead, AsyncReadExt, Take};
use tokio::{
    fs::{File, OpenOptions},
    io::{AsyncSeekExt, BufReader, BufWriter},
};
use tokio_util::codec::{FramedRead, FramedWrite};

#[derive(Debug)]
pub struct Data {
    file: File,
    path: PathBuf,
    config: SharedLogConfig,
}

impl Data {
    pub async fn open(path: impl AsRef<Path>, config: SharedLogConfig) -> Result<Self> {
        let path = path.as_ref();
        let file = OpenOptions::new().read(true).open(path).await?;
        Ok(Self {
            path: path.to_owned(),
            file,
            config,
        })
    }

    pub async fn read_slice(&self, start_position: u64, end_position: u64) -> Result<Vec<Message>> {
        let mut reader = new_reader(&self.path, start_position, end_position).await?;
        read_slice(&mut reader).await
    }
}

#[derive(Debug)]
pub struct MutData {
    path: PathBuf,
    position: u64,
    writer: FramedWrite<BufWriter<File>, LogCodec>,
    config: SharedLogConfig,
}

impl MutData {
    pub async fn open(path: impl AsRef<Path>, config: SharedLogConfig) -> Result<Self> {
        let path = path.as_ref();

        let file = OpenOptions::new().read(true).write(true).open(path).await?;
        let mut writer = BufWriter::new(file);
        writer.seek(SeekFrom::End(0)).await?;
        let position = writer.stream_position().await?;
        let writer = FramedWrite::new(writer, LogCodec);

        Ok(Self {
            path: path.to_owned(),
            writer,
            position,
            config,
        })
    }

    pub async fn create(path: impl AsRef<Path>, config: SharedLogConfig) -> Result<Self> {
        let path = path.as_ref();

        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .read(true)
            .open(path)
            .await?;

        let writer = BufWriter::new(file);
        let writer = FramedWrite::new(writer, LogCodec);

        Ok(Self {
            path: path.to_owned(),
            writer,
            position: 0,
            config,
        })
    }

    pub async fn write(&mut self, message: Message) -> Result<()> {
        let length = message.headers().length() as u64;
        self.writer.send(message).await?;
        self.writer.flush().await?;
        self.position += length;

        Ok(())
    }

    pub fn position(&self) -> u64 {
        self.position
    }

    pub async fn read_slice(&self, start_position: u64, end_position: u64) -> Result<Vec<Message>> {
        let mut reader = new_reader(&self.path, start_position, end_position).await?;
        read_slice(&mut reader).await
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

async fn read_slice(reader: &mut FramedRead<Take<File>, LogCodec>) -> Result<Vec<Message>> {
    let mut messages = vec![];

    while let Some(Ok(message)) = reader.next().await {
        messages.push(message)
    }

    Ok(messages)
}
