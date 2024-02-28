use anyhow::Result;
use bytes::Bytes;
use selium_log::{
    config::LogConfig,
    message::{Headers, Message},
    message_log::MessageLog,
};
use std::sync::Arc;

async fn write_batches(size: usize, log: &mut MessageLog) -> Result<()> {
    for i in 0..size {
        let message = format!("Some message {}", i + 1);
        let batch = Bytes::from(message);
        let headers = Headers::new(batch.len(), 1, 1);
        let message = Message::new(headers, &batch);

        log.write(message).await?;
        //tokio::time::sleep(Duration::from_secs(1)).await;
    }

    Ok(())
}

#[tokio::test]
async fn reads_log() -> Result<()> {
    let path = "./logs/topic/";
    let max_entries = 10_000;
    let config = Arc::new(LogConfig::new(max_entries, path));
    let mut log = MessageLog::open(config.clone()).await?;

    write_batches(max_entries as usize, &mut log).await?;
    drop(log);

    let log = MessageLog::open(config).await?;

    let messages = log.read_messages(0..10_000).await?;
    assert_eq!(messages.len(), max_entries as usize);

    Ok(())
}
