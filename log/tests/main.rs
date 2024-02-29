use anyhow::Result;
use bytes::Bytes;
use selium_log::{
    config::LogConfig,
    message::{Headers, Message},
    message_log::MessageLog,
};
use std::sync::Arc;
use tempfile::TempDir;

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
    let config = Arc::new(LogConfig::new(max_entries as u32, path));
    let mut log = MessageLog::open(config.clone()).await?;

    write_batches(max_entries as usize, &mut log).await?;
    drop(log);

    let log = MessageLog::open(config).await?;

    let messages = log.read_messages(0..max_entries).await?;
    assert_eq!(messages.len(), max_entries as usize - 1);

    Ok(())
}
