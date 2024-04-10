use bytes::Bytes;
use selium_log::{
    config::LogConfig,
    message::{Headers, Message},
    message_log::MessageLog,
};
use std::{sync::Arc, time::Duration};
use tempfile::TempDir;

const SEVEN_DAYS: u64 = 60 * 60 * 24 * 7;

async fn write_batches(max_entries: usize, log: &mut MessageLog) {
    for i in 0..max_entries {
        let message = format!("Some message {}", i + 1);
        let batch = Bytes::from(message);
        let headers = Headers::new(batch.len(), 1, 1);
        let message = Message::new(headers, &batch);

        log.write(message).await.unwrap();
    }
}

#[tokio::test]
async fn reads_log() {
    let tempdir = TempDir::new().unwrap();
    let path = tempdir.path();
    let max_entries = 100_000u64;
    let config = Arc::new(LogConfig::new(
        max_entries as u32,
        path,
        Duration::from_secs(SEVEN_DAYS),
        Duration::from_secs(10),
    ));

    let mut log = MessageLog::open(config).await.unwrap();

    for i in 0..=max_entries {
        let message = format!("Some message {}", i + 1);
        let batch = Bytes::from(message);
        let headers = Headers::new(batch.len(), 1, 1);
        let message = Message::new(headers, &batch);

        log.write(message).await.unwrap();
    }

    let mut offset = 0;

    loop {
        let range = offset..offset + 1000;
        let slice = log.read_slice(range).await.unwrap();

        offset = slice.end_offset();

        match slice.messages().as_mut() {
            Some(ref mut iterator) => while let Some(message) = iterator.next().await {},
            None => {
                println!("Finished reading at offset: {offset}");
                break;
            }
        }
    }
}
