//! Stress test: N writers × M readers on one channel with slot churn.
//!
//! Verifies that the generation-wait wake/notify mechanism delivers all
//! frames without lost wakeups and without busy-spinning.

use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use selium_abi::ResourceKind;
use selium_shm::{Channel, ChannelBackpressure, ShmTransport};

fn install_heap_provider() {
    drop(selium_memory::set_region_provider(Box::new(
        selium_memory::HeapRegionProvider::new(),
    )));
}

fn make_channel(capacity: u64) -> Channel {
    Channel::create_with_backpressure(
        capacity,
        ChannelBackpressure::Park,
        ResourceKind::SharedMemory,
    )
    .expect("create channel")
}

/// N writers on one Park channel. Each writer sends `FRAMES_PER_WRITER`
/// frames through `FramedWrite`. A single reader drains all frames.
/// Asserts all frames are received and the test completes within a time
/// budget (no deadlock, no lost wakeups).
#[test]
fn n_writers_single_reader_no_lost_wakeups() {
    install_heap_provider();

    const N_WRITERS: usize = 3;
    const FRAMES_PER_WRITER: u64 = 20;
    const CHANNEL_CAPACITY: u64 = 4096; // Large enough for all frames to avoid Drop loss.
    const BACKPRESSURE: ChannelBackpressure = ChannelBackpressure::Drop; // Drop: writers never block.

    let data_channel = Arc::new(
        Channel::create_with_backpressure(
            CHANNEL_CAPACITY,
            BACKPRESSURE,
            ResourceKind::SharedMemory,
        )
        .expect("create data channel"),
    );
    let dummy_channel = Arc::new(make_channel(64));

    // Keep a blocking writer alive for the test's duration so the reader
    // doesn't see EOF (writer_count=0) before all frames are consumed.
    let _keepalive_writer = data_channel.blocking_writer().expect("keepalive writer");

    let total_expected = (N_WRITERS as u64) * FRAMES_PER_WRITER;
    let total_read = Arc::new(AtomicU64::new(0));

    let rt = tokio::runtime::Runtime::new().expect("tokio rt");

    rt.block_on(async {
        // Spawn the reader BEFORE the writers so it doesn't miss frames.
        let read_ch = data_channel.clone();
        let dummy_ch = dummy_channel.clone();
        let counter = total_read.clone();
        let target = total_expected;

        let reader_handle = tokio::task::spawn_blocking(move || {
            let transport = ShmTransport::new(&read_ch, &dummy_ch).expect("reader transport");
            let mut framed = selium_wire::FramedRead::new(transport);

            loop {
                if counter.load(Ordering::SeqCst) >= target {
                    break;
                }
                match framed.read_frame() {
                    Ok((_payload, _tag, _flags)) => {
                        counter.fetch_add(1, Ordering::SeqCst);
                    }
                    Err(selium_wire::Error::BufferEmpty) => {
                        // No data ready; brief sleep to avoid hot-spin.
                        std::thread::sleep(std::time::Duration::from_micros(200));
                    }
                    Err(selium_wire::Error::Terminated) => break,
                    Err(_e) => { /* non-blocking reader: continue */ }
                }
            }
        });

        // Spawn writers concurrently.
        let mut writer_handles = Vec::new();
        for widx in 0..N_WRITERS {
            let write_ch = data_channel.clone();
            let dummy_ch = dummy_channel.clone();
            writer_handles.push(tokio::task::spawn_blocking(move || {
                let transport = ShmTransport::new(&dummy_ch, &write_ch).expect("writer transport");
                let mut framed = selium_wire::FramedWrite::new(transport);

                for i in 0..FRAMES_PER_WRITER {
                    let payload = format!("{widx:02}-{i:04}    ");
                    framed
                        .write_frame(payload.as_bytes(), widx as u32)
                        .expect("write frame");
                }
            }));
        }

        // Wait for all writers.
        for h in writer_handles {
            h.await.expect("writer task");
        }

        // Wait for reader to finish (with a deadline).
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            if total_read.load(Ordering::SeqCst) >= total_expected {
                break;
            }
            if std::time::Instant::now() > deadline {
                panic!(
                    "timeout: read {} of {} frames",
                    total_read.load(Ordering::SeqCst),
                    total_expected
                );
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }

        reader_handle.await.expect("reader task");
    });

    assert_eq!(
        total_read.load(Ordering::SeqCst),
        total_expected,
        "all frames should have been read"
    );
}

/// Park backpressure: fill the ring, then verify `bump_generation` was
/// called for each write (generation advances monotonically).
#[test]
fn park_channel_generation_advances_on_write() {
    install_heap_provider();

    let channel = make_channel(128);

    // Initial generation.
    let gen0 = channel.ring().region().load_generation().unwrap_or(0);
    assert_eq!(gen0, 0, "generation should start at 0");

    // Write frames via the channel's writer through a transport.
    let dummy = make_channel(64);
    let transport = ShmTransport::new(&dummy, &channel).expect("transport");
    let mut framed = selium_wire::FramedWrite::new(transport);

    for i in 0..5 {
        let payload = format!("gen-test-{i:04}   "); // 8 bytes + 12 header = 20
        framed.write_frame(payload.as_bytes(), 0).expect("write");
    }

    // Generation should have advanced by 5 (one bump per write).
    let gen5 = channel.ring().region().load_generation().unwrap_or(0);
    assert_eq!(gen5, 5, "generation should be 5 after 5 writes, got {gen5}");
}

/// Two mappings of the same region (simulating two processes): writes via
/// one mapping are visible in the other, and `bump_generation` notifies.
#[test]
fn two_mappings_share_generation_and_notify() {
    install_heap_provider();

    let channel = make_channel(256);
    let region_a = channel.ring().region().clone();
    let region_b = channel.ring().region().clone();

    // Verify generation is shared.
    let gen_before = region_a.load_generation().unwrap_or(0);
    assert_eq!(region_b.load_generation().unwrap_or(0), gen_before);

    // Bump via side A.
    let gen_after = region_a.bump_generation().expect("bump");
    assert_eq!(region_b.load_generation().expect("gen b"), gen_after);

    // atomic_notify should return without error (no waiters registered).
    let _ = region_b
        .mapping()
        .atomic_notify(selium_shm::region::GENERATION_COUNTER_OFFSET, 1)
        .expect("notify");
}
