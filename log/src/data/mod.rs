mod message;

pub use message::Message;

use crate::config::SharedLogConfig;
use anyhow::Result;
use std::path::{Path, PathBuf};
use tokio::{
    fs::{File, OpenOptions},
    io::{AsyncSeekExt, AsyncWriteExt, BufWriter},
};

#[derive(Debug)]
pub struct Data {
    path: PathBuf,
    file: File,
    config: SharedLogConfig,
}

impl Data {
    pub async fn open(path: impl AsRef<Path>, config: SharedLogConfig) -> Result<Self> {
        let path = path.as_ref();
        let file = OpenOptions::new().read(true).open(&path).await?;

        Ok(Self {
            path: path.to_owned(),
            file,
            config,
        })
    }
}

#[derive(Debug)]
pub struct MutData {
    path: PathBuf,
    position: u64,
    writer: BufWriter<File>,
    config: SharedLogConfig,
}

impl MutData {
    pub async fn open(path: impl AsRef<Path>, config: SharedLogConfig) -> Result<Self> {
        let path = path.as_ref();

        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .await?;

        let mut writer = BufWriter::new(file);
        let position = writer.stream_position().await?;

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
            .write(true)
            .read(true)
            .open(&path)
            .await?;

        let writer = BufWriter::new(file);

        Ok(Self {
            path: path.to_owned(),
            writer,
            position: 0,
            config,
        })
    }

    pub async fn write(&mut self, message: Message) -> Result<()> {
        let serialized = bincode::serialize(&message)?;
        let length = serialized.len();

        self.writer.write_all(&serialized).await?;
        self.writer.flush().await?;
        self.position += length as u64;

        Ok(())
    }

    pub fn position(&self) -> u64 {
        self.position
    }
}
