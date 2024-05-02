use bytes::Bytes;
use criterion::{criterion_group, criterion_main, Criterion};
use selium_log::{
    config::{FlushPolicy, LogConfig},
    message::Message,
    MessageLog,
};
use std::{sync::Arc, time::Duration};
use tempfile::tempdir;

const ONE_DAY: u64 = 86_400;
const NUM_OF_MESSAGES: u64 = 1_000_000;
const MAX_ENTRIES_PER_SEGMENT: u32 = 50_000;

fn get_log_config() -> LogConfig {
    let tempdir = tempdir().unwrap();

    LogConfig::from_path(tempdir.path())
        .max_index_entries(MAX_ENTRIES_PER_SEGMENT)
        .retention_period(Duration::from_secs(ONE_DAY))
        .cleaner_interval(Duration::from_secs(ONE_DAY))
        .flush_policy(FlushPolicy::default().number_of_writes(100))
}

async fn log_task() {
    let config = get_log_config();
    let log = MessageLog::open(Arc::new(config)).await.unwrap();

    for _ in 0..NUM_OF_MESSAGES {
        let batch = Bytes::copy_from_slice(&vec![1; 32]);
        let message = Message::single(&batch, 1);
        log.write(message).await.unwrap();
    }

    log.flush().await.unwrap();
}

pub fn benchmark(c: &mut Criterion) {
    c.bench_function("write 1_000_000 records", |b| {
        let runtime = tokio::runtime::Runtime::new().expect("Failed to construct executor");
        b.to_async(runtime).iter(|| log_task());
    });
}

criterion_group! {
    name = benches;
    config = Criterion::default().sample_size(10);
    targets = benchmark
}
criterion_main!(benches);
