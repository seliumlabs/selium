use anyhow::Result;
use bytes::Bytes;
use selium_log::{
    config::LogConfig,
    message::{Headers, Message},
    message_log::MessageLog,
};
use std::{sync::Arc, time::Duration};
use tempfile::TempDir;

const SEVEN_DAYS: u64 = 60 * 60 * 24 * 7;

async fn write_batches(max_entries: usize, log: &mut MessageLog) -> Result<()> {
    for i in 0..max_entries {
        let message = format!("Some message {}", i + 1);
        let batch = Bytes::from(message);
        let headers = Headers::new(batch.len(), 1, 1);
        let message = Message::new(headers, &batch);

        log.write(message).await?;
    }

    Ok(())
}

#[tokio::test]
async fn reads_log() -> Result<()> {
    let tempdir = TempDir::new().unwrap();
    let path = tempdir.path();
    let max_entries = 100u64;
    let config = Arc::new(LogConfig::new(
        max_entries as u32,
        path,
        Duration::from_secs(SEVEN_DAYS),
        Duration::from_secs(10),
    ));
    let mut log = MessageLog::open(config.clone()).await?;

    write_batches(max_entries as usize, &mut log).await?;
    drop(log);

    let log = MessageLog::open(config).await?;
    let slice = log.read_slice(0..max_entries).await?;

    assert_eq!(slice.messages().len(), max_entries as usize - 1);
    assert_eq!(slice.end_offset(), max_entries);

    Ok(())
}
