mod helpers;

use helpers::TestWrapper;
use selium_log::config::{FlushPolicy, LogConfig};
use std::{ops::Add, time::Duration};
use tempfile::TempDir;

#[tokio::test]
async fn writes_to_log() {
    let total_messages = 10_000;
    let tempdir = TempDir::new().unwrap();
    let config = LogConfig::from_path(tempdir.path());
    let mut wrapper = TestWrapper::build(config).await;

    for _ in 0..total_messages {
        wrapper.write_batch("Hello, world").await;
    }

    wrapper.flush().await;

    let mut messages = wrapper.read_records(0, None).await.unwrap();
    let mut read_messages = 0;

    while let Some(_) = messages.next().await {
        read_messages += 1;
    }

    assert_eq!(read_messages, total_messages);
}

#[tokio::test]
async fn writes_producer_messages_in_order() {
    let total_messages = 10_000;

    let messages = (0..total_messages)
        .map(|i| format!("Hello, world {i}"))
        .collect::<Vec<_>>();

    let tempdir = TempDir::new().unwrap();
    let config = LogConfig::from_path(tempdir.path());
    let mut wrapper = TestWrapper::build(config).await;

    for message in messages.iter() {
        wrapper.write_batch(message).await;
    }

    wrapper.flush().await;

    let mut read_messages = vec![];
    let mut slice = wrapper.read_records(0, None).await.unwrap();

    while let Some(Ok(message)) = slice.next().await {
        let message = String::from_utf8(message.records().to_vec()).unwrap();
        read_messages.push(message);
    }

    assert_eq!(messages, read_messages);
}

#[tokio::test]
async fn splits_log_into_segments() {
    let max_index_entries = 10_000;
    let number_of_segments = 10;
    let total_messages = (max_index_entries * number_of_segments) - 1;

    let tempdir = TempDir::new().unwrap();
    let config = LogConfig::from_path(tempdir.path()).max_index_entries(max_index_entries);
    let mut wrapper = TestWrapper::build(config).await;

    for _ in 0..total_messages {
        wrapper.write_batch("Hello, world").await;
    }

    wrapper.flush().await;

    let actual_number_of_segments = wrapper.number_of_segments().await;
    assert_eq!(actual_number_of_segments, number_of_segments as u64);
}

#[tokio::test]
async fn flushes_log_based_on_interval() {
    let total_messages = 10_000;
    let flush_interval = Duration::from_secs(3);
    let flush_policy = FlushPolicy::default().interval(flush_interval);
    let tempdir = TempDir::new().unwrap();

    let config = LogConfig::from_path(tempdir.path()).flush_policy(flush_policy);
    let mut runner = TestWrapper::build(config).await;

    for _ in 0..total_messages {
        runner.write_batch("Hello, world").await;
    }

    // Flushes are performed asynchronously, so the records shouldn't be flushed yet.
    let first = runner.read_records(0, None).await.unwrap().next().await;
    assert!(first.is_none());

    tokio::time::sleep(flush_interval).await;

    // The interval has elapsed, so the buffer should've flushed to the filesystem now.
    let first = runner.read_records(0, None).await.unwrap().next().await;
    assert!(first.is_some());
}

#[tokio::test]
async fn flushes_log_based_on_writes() {
    let number_of_writes_before_flush = 10_000;
    let flush_policy = FlushPolicy::default().number_of_writes(number_of_writes_before_flush);

    let tempdir = TempDir::new().unwrap();
    let config = LogConfig::from_path(tempdir.path()).flush_policy(flush_policy);
    let mut runner = TestWrapper::build(config).await;

    for _ in 0..(number_of_writes_before_flush - 1) {
        runner.write_batch("Hello, world").await;
    }

    // Number of writes prior to flush has not been exceeded, so the records shouldn't be flushed yet.
    let first = runner.read_records(0, None).await.unwrap().next().await;
    assert!(first.is_none());

    // The final write should trigger the flush threshold.
    runner.write_batch("Hello, world").await;
    let first = runner.read_records(0, None).await.unwrap().next().await;
    assert!(first.is_some());
}

#[tokio::test]
async fn removes_stale_logs() {
    let max_index_entries = 10_000;
    let number_of_segments = 10;
    let total_messages = (max_index_entries * number_of_segments) - 1;
    let retention_period = Duration::from_secs(3);
    let cleaner_interval = Duration::from_secs(1);

    let tempdir = TempDir::new().unwrap();
    let config = LogConfig::from_path(tempdir.path())
        .max_index_entries(max_index_entries)
        .retention_period(retention_period)
        .cleaner_interval(cleaner_interval);

    let mut runner = TestWrapper::build(config).await;

    for _ in 0..total_messages {
        runner.write_batch("Hello, world").await;
    }

    runner.flush().await;

    // Prior to the retention period elapsing, no segments should be removed yet.
    let written_segments = runner.number_of_segments().await;
    assert_eq!(written_segments, number_of_segments as u64);

    tokio::time::sleep(retention_period.add(Duration::from_secs(1))).await;

    // All segments are stale and should be removed now.
    let written_segments = runner.number_of_segments().await;
    assert_eq!(written_segments, 0);
}
