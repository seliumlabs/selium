mod helpers;

use helpers::generate_dummy_messages;
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

    wrapper.write_dummy_records(total_messages).await;
    wrapper.flush().await;

    let messages = wrapper.read_records(0, None).await;
    assert_eq!(messages.len(), total_messages);
}

#[tokio::test]
async fn writes_producer_messages_in_order() {
    let total_messages = 10_000;
    let messages = generate_dummy_messages(total_messages);

    let tempdir = TempDir::new().unwrap();
    let config = LogConfig::from_path(tempdir.path());
    let mut wrapper = TestWrapper::build(config).await;

    wrapper.write_records(messages.as_slice()).await;
    wrapper.flush().await;

    let read_messages = wrapper.read_records(0, None).await;
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

    wrapper.write_dummy_records(total_messages as usize).await;
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
    let mut wrapper = TestWrapper::build(config).await;
    wrapper.write_dummy_records(total_messages as usize).await;

    // Flushes are performed asynchronously, so the records shouldn't be flushed yet.
    let messages = wrapper.read_records(0, None).await;
    assert!(messages.is_empty());

    tokio::time::sleep(flush_interval).await;

    // The interval has elapsed, so the buffer should've flushed to the filesystem now.
    let messages = wrapper.read_records(0, None).await;
    assert!(!messages.is_empty());
}

#[tokio::test]
async fn flushes_log_based_on_writes() {
    let number_of_writes_before_flush = 10_000;
    let flush_policy = FlushPolicy::default().number_of_writes(number_of_writes_before_flush);

    let tempdir = TempDir::new().unwrap();
    let config = LogConfig::from_path(tempdir.path()).flush_policy(flush_policy);
    let mut wrapper = TestWrapper::build(config).await;

    wrapper
        .write_dummy_records(number_of_writes_before_flush as usize - 1)
        .await;

    // Number of writes prior to flush has not been exceeded, so the records shouldn't be flushed yet.
    let messages = wrapper.read_records(0, None).await;
    assert!(messages.is_empty());

    // The final write should trigger the flush threshold.
    wrapper.write_dummy_records(1).await;
    let messages = wrapper.read_records(0, None).await;
    assert!(!messages.is_empty());
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

    let mut wrapper = TestWrapper::build(config).await;
    wrapper.write_dummy_records(total_messages as usize).await;
    wrapper.flush().await;

    // Prior to the retention period elapsing, no segments should be removed yet.
    let written_segments = wrapper.number_of_segments().await;
    assert_eq!(written_segments, number_of_segments as u64);

    tokio::time::sleep(retention_period.add(Duration::from_secs(1))).await;

    // All segments are stale and should be removed now.
    let written_segments = wrapper.number_of_segments().await;
    assert_eq!(written_segments, 0);
}

#[tokio::test]
async fn fetches_all_messages_in_segment_if_no_limit_specified() {
    let tempdir = TempDir::new().unwrap();
    let config = LogConfig::from_path(tempdir.path());
    let total_messages = config.max_index_entries;
    let mut wrapper = TestWrapper::build(config).await;

    wrapper.write_dummy_records(total_messages as usize).await;
    wrapper.flush().await;

    let messages = wrapper.read_records(0, None).await;
    assert_eq!(messages.len(), total_messages as usize);
}

#[tokio::test]
async fn fetches_x_messages_in_segment_if_limit_specified() {
    let tempdir = TempDir::new().unwrap();
    let config = LogConfig::from_path(tempdir.path());
    let total_messages = config.max_index_entries;
    let limit = total_messages as u64 / 10;
    let mut wrapper = TestWrapper::build(config).await;

    wrapper.write_dummy_records(total_messages as usize).await;
    wrapper.flush().await;

    let messages = wrapper.read_records(0, Some(limit)).await;
    assert_eq!(messages.len(), limit as usize);
}

#[tokio::test]
async fn fetches_all_messages_if_limit_greater_than_total() {
    let tempdir = TempDir::new().unwrap();
    let config = LogConfig::from_path(tempdir.path());
    let total_messages = config.max_index_entries / 10;
    let limit = config.max_index_entries as u64;
    let mut wrapper = TestWrapper::build(config).await;

    wrapper.write_dummy_records(total_messages as usize).await;
    wrapper.flush().await;

    let messages = wrapper.read_records(0, Some(limit)).await;
    assert_eq!(messages.len(), total_messages as usize);
}

#[tokio::test]
async fn fetches_messages_starting_from_offset() {
    let offset = 1_000;
    let total_messages = offset * 10;
    let max_entries = total_messages * 10;

    let tempdir = TempDir::new().unwrap();
    let config = LogConfig::from_path(tempdir.path()).max_index_entries(max_entries);
    let mut wrapper = TestWrapper::build(config).await;

    let messages = generate_dummy_messages(total_messages as usize);
    wrapper.write_records(messages.as_slice()).await;
    wrapper.flush().await;

    let read_messages = wrapper.read_records(offset as u64, None).await;
    let (_, offset_messages) = messages.split_at(offset as usize);

    assert_eq!(read_messages.len(), offset_messages.len());
}

#[tokio::test]
async fn fetches_messages_from_offset_with_limit() {
    let offset = 1_000;
    let limit = 1_000;

    let total_messages = offset * 10;
    let max_entries = total_messages * 10;

    let tempdir = TempDir::new().unwrap();
    let config = LogConfig::from_path(tempdir.path()).max_index_entries(max_entries);
    let mut wrapper = TestWrapper::build(config).await;

    let messages = generate_dummy_messages(total_messages as usize);
    wrapper.write_records(messages.as_slice()).await;
    wrapper.flush().await;

    let read_messages = wrapper
        .read_records(offset as u64, Some(limit as u64))
        .await;

    let offset_messages = messages
        .into_iter()
        .skip(offset as usize)
        .take(limit as usize)
        .collect::<Vec<_>>();

    assert_eq!(read_messages.len(), offset_messages.len());
}
