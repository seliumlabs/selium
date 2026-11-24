//! Cross-wrapper frame conformance: every ring surface must apply the same
//! canonical validation from `selium_shm::layout`.

use std::pin::Pin;
use std::task::{Context, Poll};

use selium_memory::{FrameHeader, HeapRegionProvider, set_region_provider};
use selium_shm::{Channel, ChannelBackpressure, layout, ring_buf::RingBuf};
use selium_wire::error::Error;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

fn setup() {
    drop(set_region_provider(Box::new(HeapRegionProvider::new())));
}

fn channel() -> Channel {
    Channel::create_with_backpressure(
        64,
        ChannelBackpressure::Park,
        selium_abi::ResourceKind::SharedMemory,
    )
    .expect("create channel")
}

fn encoded(payload_len: usize, header_len: u32) -> Vec<u8> {
    let header = FrameHeader {
        len: header_len,
        tag: 7,
        flags: FrameHeader::FLAG_READY,
    };
    let mut bytes = header.encode().to_vec();
    bytes.extend(std::iter::repeat_n(0xAB, payload_len));
    bytes
}

// scenario: ring-protocol-core/wrappers-cannot-diverge-from-canonical-validation/oversized-encoded-frame-rejected-uniformly
#[test]
fn oversized_encoded_frame_is_rejected_by_every_writer() {
    setup();
    let channel = channel();
    let region = channel.ring().region();
    let oversized = encoded(100, 100);

    // Canonical primitive.
    assert!(matches!(
        layout::write_encoded_frame_at(
            region.mapping().backend(),
            0,
            &oversized,
            region.capacity(),
            region.capacity() - 1,
        ),
        Err(Error::CapacityExceeded)
    ));

    // RingBuf.
    assert!(matches!(
        channel.ring().write_frame(0, &[0u8; 100], 0, 0),
        Err(Error::CapacityExceeded)
    ));

    // Async writer wrapper.
    let mut writer = channel.writer().expect("writer");
    let waker = std::task::Waker::noop();
    let mut cx = Context::from_waker(waker);
    let poll = Pin::new(&mut writer).poll_write(&mut cx, &oversized);
    assert!(matches!(poll, Poll::Ready(Err(_))));
}

// scenario: ring-protocol-core/wrappers-cannot-diverge-from-canonical-validation/encoded-buffer-length-disagrees-with-header
#[test]
fn encoded_length_must_match_the_header_on_every_writer() {
    setup();
    let channel = channel();
    let region = channel.ring().region();
    // Header declares 5 payload bytes; the buffer carries 3.
    let mismatched = encoded(3, 5);

    assert!(matches!(
        layout::write_encoded_frame_at(
            region.mapping().backend(),
            0,
            &mismatched,
            region.capacity(),
            region.capacity() - 1,
        ),
        Err(Error::InvalidFrame(_))
    ));

    let mut writer = channel.writer().expect("writer");
    let waker = std::task::Waker::noop();
    let mut cx = Context::from_waker(waker);
    let poll = Pin::new(&mut writer).poll_write(&mut cx, &mismatched);
    assert!(
        matches!(poll, Poll::Ready(Err(_))),
        "the wrapper must reject a header/payload length disagreement"
    );
}

fn assert_channel_reader_error<R: AsyncRead + Unpin>(
    reader: &mut R,
    cx: &mut Context<'_>,
    canonical_message: &str,
) {
    let mut storage = [0u8; 64];
    let mut buffer = ReadBuf::new(&mut storage);
    let poll = Pin::new(reader).poll_read(cx, &mut buffer);
    assert!(
        matches!(poll, Poll::Ready(Err(_))),
        "channel reader must reject the frame"
    );
    let Poll::Ready(Err(error)) = poll else {
        return;
    };
    let source = error
        .get_ref()
        .and_then(|source| source.downcast_ref::<Error>())
        .expect("channel reader preserves the canonical wire error");
    assert!(
        matches!(source, Error::InvalidFrame(message) if message == canonical_message),
        "channel reader diverged from canonical validation: {source:?}"
    );
}

// scenario: selium-shm/layout-plumbing-separation/public-api-stability
// scenario: ring-protocol-core/wrappers-cannot-diverge-from-canonical-validation/protocol-regression-applies-to-all-wrappers
#[test]
fn oversized_frame_in_the_ring_is_rejected_by_every_reader() {
    setup();
    let channel = channel();
    let region = channel.ring().region();
    let capacity = region.capacity();
    let mut plain = channel.reader();
    let mut blocking = channel.blocking_reader_from(0).expect("blocking reader");

    // A checksum-valid header claiming a frame larger than the ring.
    let header = FrameHeader {
        len: u32::try_from(capacity).expect("capacity fits u32"),
        tag: 0,
        flags: FrameHeader::FLAG_READY,
    };
    let ring = RingBuf::wrap_region(region.clone()).expect("wrap");
    ring.write_at(0, &header.encode()).expect("seed header");
    region
        .write_next_tail(FrameHeader::ENCODED_SIZE as u64)
        .expect("publish seeded header");

    let canonical = layout::read_frame(region.mapping().backend(), 0, capacity - 1, capacity)
        .expect_err("canonical reader rejects the frame");
    let Error::InvalidFrame(canonical_message) = canonical else {
        panic!("expected canonical InvalidFrame")
    };
    assert!(matches!(
        ring.read_frame(0),
        Err(Error::InvalidFrame(message)) if message == canonical_message
    ));

    let waker = std::task::Waker::noop();
    let mut cx = Context::from_waker(waker);
    assert_channel_reader_error(&mut plain, &mut cx, &canonical_message);
    assert_channel_reader_error(&mut blocking, &mut cx, &canonical_message);
}
