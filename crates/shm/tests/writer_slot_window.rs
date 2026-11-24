//! Blocking-writer slot window regressions.
//!
//! A blocking writer's slot anchors *other* writers; it must never pin the
//! writer itself, and it must advance with the frames it commits rather than
//! staying at the first frame's start. These tests pin both halves of that
//! contract on a ring small enough to expose the arithmetic.

use std::task::{Context, Poll};

use selium_abi::ResourceKind;
use selium_memory::FrameHeader;
use selium_shm::{Channel, ChannelBackpressure};
use tokio::io::{AsyncWrite, AsyncWriteExt};

#[tokio::test]
async fn a_fresh_writer_can_write_after_pre_existing_ring_data_is_drained() {
    install_heap_provider();
    let channel = park_channel(64);
    let mut reader = channel.blocking_reader().expect("reader");
    let bytes = frame(36);

    // Data written by a peer before this writer existed (modelled with the
    // allocation's ring handle, i.e. no writer slot of its own).
    let required = bytes.len() as u64;
    let position = channel.ring().reserve(required).expect("prefill reserve");
    channel
        .ring()
        .write_frame(position, &bytes[FrameHeader::ENCODED_SIZE..], 0, 0)
        .expect("prefill frame");
    let mut buf = [0u8; 128];
    assert_eq!(read_one_frame(&mut reader, &mut buf).await, bytes.len());

    // The fresh writer's own slot sits at 0; it must not block the first write
    // once the pre-existing data has been consumed.
    let mut writer = channel.blocking_writer().expect("writer");
    match poll_write_once(&mut writer, &bytes) {
        Poll::Ready(Ok(written)) => assert_eq!(written, bytes.len()),
        other => panic!("a fresh writer must not be pinned by its own slot, got {other:?}"),
    }
}

// scenario: channel-wake-wait/wakeable-channel-writes-park-backpressure/a-large-frame-does-not-pin-its-own-writer
#[tokio::test]
async fn a_large_frame_does_not_pin_its_own_writer() {
    install_heap_provider();
    // A 52-byte frame on a 64-byte ring is more than half the ring: with the
    // writer's own slot counted, the second write could never fit.
    let channel = park_channel(64);
    let mut reader = channel.blocking_reader().expect("reader");
    let mut writer = channel.blocking_writer().expect("writer");
    let bytes = frame(36);

    writer.write_all(&bytes).await.expect("first frame");
    let mut buf = [0u8; 128];
    assert_eq!(read_one_frame(&mut reader, &mut buf).await, bytes.len());

    match poll_write_once(&mut writer, &bytes) {
        Poll::Ready(Ok(written)) => assert_eq!(written, bytes.len()),
        other => panic!("second write must succeed after the drain, got {other:?}"),
    }
}

// scenario: channel-wake-wait/wakeable-channel-writes-park-backpressure/an-idle-writer-still-anchors-other-writers
#[tokio::test]
async fn an_idle_writer_still_anchors_other_writers() {
    install_heap_provider();
    // A slow blocking writer that has written one frame must still bound how
    // far other writers may run ahead (the documented Park backpressure).
    let channel = park_channel(64);
    let mut reader = channel.blocking_reader().expect("reader");
    let mut slow = channel.blocking_writer().expect("slow writer");
    let mut fast = channel.blocking_writer().expect("fast writer");
    let bytes = frame(20);

    slow.write_all(&bytes).await.expect("slow frame");
    let mut buf = [0u8; 128];
    assert_eq!(read_one_frame(&mut reader, &mut buf).await, bytes.len());

    // The fast writer may fill up to one capacity past the slow writer's
    // anchor, and then parks instead of overwriting it.
    let mut written_frames = 0usize;
    loop {
        match poll_write_once(&mut fast, &bytes) {
            Poll::Ready(Ok(_)) => {
                written_frames += 1;
                assert!(read_one_frame(&mut reader, &mut buf).await > 0);
                assert!(
                    written_frames < 16,
                    "an idle blocking writer must bound the window, not admit unbounded traffic"
                );
            }
            Poll::Ready(Err(error)) => panic!("unexpected write error: {error}"),
            Poll::Pending => break,
        }
    }
    assert!(
        written_frames > 0,
        "the fast writer must make progress inside the window"
    );
}

/// Encodes one complete ring frame: header + payload.
fn frame(payload: usize) -> Vec<u8> {
    let mut frame = FrameHeader {
        len: payload as u32,
        tag: 0,
        flags: FrameHeader::FLAG_READY,
    }
    .encode()
    .to_vec();
    frame.extend_from_slice(&vec![b'x'; payload]);
    frame
}

fn install_heap_provider() {
    drop(selium_memory::set_region_provider(Box::new(
        selium_memory::HeapRegionProvider::new(),
    )));
}

fn park_channel(capacity: u64) -> Channel {
    Channel::create_with_backpressure(
        capacity,
        ChannelBackpressure::Park,
        ResourceKind::SharedMemory,
    )
    .expect("create channel")
}

fn poll_write_once(
    writer: &mut selium_shm::channels::BlockingWriter,
    bytes: &[u8],
) -> Poll<std::io::Result<usize>> {
    let waker = futures::task::noop_waker();
    let mut cx = Context::from_waker(&waker);
    std::pin::Pin::new(writer).poll_write(&mut cx, bytes)
}

/// Reads one complete frame (header + payload) from the blocking reader.
async fn read_one_frame(
    reader: &mut selium_shm::channels::BlockingReader,
    buf: &mut [u8],
) -> usize {
    use tokio::io::AsyncRead;
    std::future::poll_fn(|cx| {
        let mut read_buf = tokio::io::ReadBuf::new(buf);
        match std::pin::Pin::new(&mut *reader).poll_read(cx, &mut read_buf) {
            std::task::Poll::Ready(Ok(())) => std::task::Poll::Ready(Ok(read_buf.filled().len())),
            std::task::Poll::Ready(Err(error)) => std::task::Poll::Ready(Err(error)),
            std::task::Poll::Pending => std::task::Poll::Pending,
        }
    })
    .await
    .expect("frame read")
}

// --- Spec traceability (scripts/check-traceability.sh) -------------------
