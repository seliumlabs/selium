use bytes::Bytes;
use criterion::{black_box, criterion_group, criterion_main, Criterion};
use selium_log::{
    config::{FlushPolicy, LogConfig},
    message::{Headers, Message},
    MessageLog,
};
use std::{path::Path, sync::Arc, time::Duration};
use tempfile::tempdir;

const ONE_DAY: u64 = 86_400;
const NUM_OF_MESSAGES: u64 = 1_000_000;
const MAX_ENTRIES_PER_SEGMENT: u32 = 50_000;

fn get_log_config(path: impl AsRef<Path>) -> LogConfig {
    LogConfig::new(
        MAX_ENTRIES_PER_SEGMENT,
        path,
        Duration::from_secs(ONE_DAY),
        Duration::from_secs(ONE_DAY),
        FlushPolicy::default().number_of_writes(100),
    )
}

async fn write_records(path: impl AsRef<Path>) {
    let config = get_log_config(path);
    let mut log = MessageLog::open(Arc::new(config)).await.unwrap();

    for i in 0..NUM_OF_MESSAGES {
        let message = format!("Hello, world! {i}");
        let batch = Bytes::from(message);
        let headers = Headers::new(batch.len(), 1, 1);
        let message = Message::new(headers, &batch);

        log.write(message).await.unwrap();
    }

    log.flush().await.unwrap();
}

async fn read_records(path: impl AsRef<Path>) {
    let config = get_log_config(path);
    let log = MessageLog::open(Arc::new(config)).await.unwrap();
    let mut offset = 0;

    loop {
        let slice = log.read_slice(offset, None).await.unwrap();
        offset = slice.end_offset();

        match slice.messages().as_mut() {
            Some(ref mut iterator) => {
                while let Some(next) = iterator.next().await {
                    println!("{next:?}");
                    black_box(next).unwrap();
                }
            }
            None => break,
        }
    }
}

pub fn benchmark(c: &mut Criterion) {
    let tempdir = tempdir().unwrap();
    let path = tempdir.path();
    let runtime = tokio::runtime::Runtime::new().expect("Failed to construct executor");

    runtime.block_on(async {
        write_records(path).await;
    });

    c.bench_function("read 1_000_000 records", |b| {
        let runtime = tokio::runtime::Runtime::new().expect("Failed to construct executor");
        b.to_async(runtime).iter(move || read_records(path));
    });
}

criterion_group! {
    name = benches;
    config = Criterion::default().sample_size(10);
    targets = benchmark
}
criterion_main!(benches);
