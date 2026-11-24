//! Deterministic point-to-point peer-lifecycle regressions.
//!
//! These tests force the adverse interleavings that used to make a newly
//! allocated stream look closed: initial peer absence, attach-then-close before
//! the first poll, and a final frame that must be drained before EOF. The
//! attached (legacy/many-to-many) behaviour is asserted alongside so the opt-in
//! lifecycle cannot silently change ordinary channel semantics.

use std::{
    pin::Pin,
    sync::{
        Arc, Mutex, Once,
        atomic::{AtomicUsize, Ordering},
    },
    task::{Context, Poll, Wake, Waker},
};

use selium_abi::ResourceKind;
use selium_memory::FrameHeader;
use selium_shm::{
    Channel, ChannelBackpressure,
    channels::{BlockingReader, PeerLifecycle},
};
use tokio::io::{AsyncRead, ReadBuf};

static INSTALL: Once = Once::new();
/// Wakers registered by the ring's generation-wait path, used to prove that
/// attachment alone wakes a parked reader.
static WAITS: Mutex<Vec<(u64, u64, Waker)>> = Mutex::new(Vec::new());

/// Waker that only counts wakes; used where a wake must *not* happen.
struct CountingWaker(AtomicUsize);

impl Wake for CountingWaker {
    fn wake(self: Arc<Self>) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

// scenario: peer-lifecycle/accepting-peers-start-attached/sender-closes-before-handoff-processing
#[test]
fn accepting_peer_starts_attached() {
    install_heap_provider();
    let channel = park_channel(64);
    let writer = channel.blocking_writer().expect("writer");
    drop(writer);

    // The rendezvous recipient (already-attached lifecycle) must observe the
    // sender's prior existence without waiting for a redundant attachment.
    let mut reader = channel.blocking_reader().expect("accept-side reader");
    let (result, filled) = poll_read_once(&mut reader, &noop_waker());
    assert!(
        matches!(result, Poll::Ready(Ok(()))),
        "closure after handoff"
    );
    assert!(filled.is_empty());
    assert!(reader.peer_closed().expect("peer close"));
}

// scenario: selium-shm/reader-writer-wait-semantics/pending-implies-registered
// scenario: peer-lifecycle/peer-attachment-is-observable/peer-attaches-while-remaining-idle
// scenario: channel-wake-wait/writer-attachment-wakes-channel-readers/reader-wakes-on-attachment-without-data
#[test]
fn attachment_without_data_wakes_a_parked_reader() {
    install_heap_provider();
    install_generation_callbacks();
    let channel = park_channel(64);
    let mut reader = channel
        .blocking_reader_awaiting_peer()
        .expect("awaiting reader");

    let (counter, waker) = counting_waker();
    let (result, _filled) = poll_read_once(&mut reader, &waker);
    assert!(matches!(result, Poll::Pending));
    assert_eq!(counter.0.load(Ordering::SeqCst), 0, "parked, not spinning");

    // Registration publishes the attachment through the generation counter.
    let writer = channel.blocking_writer().expect("peer attaches");
    assert!(
        counter.0.load(Ordering::SeqCst) > 0,
        "attachment alone must wake the parked reader"
    );

    let (result, filled) = poll_read_once(&mut reader, &waker);
    assert!(
        matches!(result, Poll::Pending),
        "an attached but idle peer keeps the stream open"
    );
    assert!(filled.is_empty());
    assert_eq!(reader.lifecycle(), PeerLifecycle::Attached);
    assert!(!reader.peer_closed().expect("peer close"));

    drop(writer);
    let (result, _filled) = poll_read_once(&mut reader, &waker);
    assert!(matches!(result, Poll::Ready(Ok(()))), "withdrawal is EOF");
    assert!(reader.peer_closed().expect("peer close"));
}

fn counting_waker() -> (Arc<CountingWaker>, Waker) {
    let counter = Arc::new(CountingWaker(AtomicUsize::new(0)));
    let waker = Waker::from(Arc::clone(&counter));
    (counter, waker)
}

#[test]
fn frame_commit_wakes_a_parked_reader() {
    install_heap_provider();
    install_generation_callbacks();
    let channel = park_channel(4096);
    let mut reader = channel
        .blocking_reader_awaiting_peer()
        .expect("awaiting reader");
    let _writer = channel.blocking_writer().expect("peer attaches");

    // Park on an attached-but-empty ring.
    let (counter, waker) = counting_waker();
    let (result, _filled) = poll_read_once(&mut reader, &waker);
    assert!(
        matches!(result, Poll::Pending),
        "empty ring parks the reader"
    );
    let parked_wakes = counter.0.load(Ordering::SeqCst);

    // A committed frame must publish its generation advance: the bump alone
    // leaves the parked reader asleep until an unrelated event re-enters the
    // guest, which is the regression this test pins.
    write_frame(&channel, b"hello");
    assert!(
        counter.0.load(Ordering::SeqCst) > parked_wakes,
        "committing a frame must wake a reader parked on the ring"
    );

    let (result, filled) = poll_read_once(&mut reader, &waker);
    assert!(matches!(result, Poll::Ready(Ok(()))), "frame is readable");
    assert!(
        filled.ends_with(b"hello"),
        "the committed payload is readable: {filled:?}"
    );
}

// scenario: channel-wake-wait/writer-attachment-wakes-channel-readers/writer-registration-fails
#[test]
fn failed_writer_registration_publishes_nothing() {
    install_heap_provider();
    let channel = park_channel(64);
    let region = channel.ring().region();
    let generation_before = region.load_generation().expect("generation");

    // Exhaust the writer slot table so registration fails after it has taken
    // the writer count.
    for _ in 0..selium_shm::layout::MAX_WRITER_SLOTS {
        let _slot = region
            .allocate_writer_slot(0)
            .expect("exhaust writer slots");
    }
    assert!(
        channel.blocking_writer().is_err(),
        "registration must fail with no slot available"
    );

    assert_eq!(region.load_writer_count().expect("writer count"), 0);
    assert_eq!(
        region.load_generation().expect("generation"),
        generation_before,
        "failed registration must not publish an attachment"
    );
}

// scenario: peer-lifecycle/buffered-data-precedes-eof/peer-writes-a-final-frame-and-closes
#[test]
fn final_frame_precedes_eof() {
    install_heap_provider();
    let channel = park_channel(256);
    let mut reader = channel
        .blocking_reader_from_awaiting_peer(0)
        .expect("awaiting reader");
    let writer = channel.blocking_writer().expect("writer");
    write_frame(&channel, b"last");
    drop(writer);
    let (result, filled) = poll_read_once(&mut reader, &noop_waker());
    assert!(matches!(result, Poll::Ready(Ok(()))));
    assert!(
        filled.ends_with(b"last"),
        "committed frame must arrive first, got {filled:?}"
    );

    let (result, filled) = poll_read_once(&mut reader, &noop_waker());
    assert!(matches!(result, Poll::Ready(Ok(()))), "EOF after drain");
    assert!(filled.is_empty());
}

// scenario: peer-lifecycle/initial-peer-absence-is-not-closure/relay-starts-before-its-serving-peer
#[test]
fn initial_peer_absence_is_not_closure() {
    install_heap_provider();
    let channel = park_channel(64);
    let mut reader = channel
        .blocking_reader_awaiting_peer()
        .expect("awaiting reader");

    let (result, filled) = poll_read_once(&mut reader, &noop_waker());
    assert!(matches!(result, Poll::Pending), "initial absence must park");
    assert!(filled.is_empty());
    assert_eq!(
        reader.lifecycle(),
        PeerLifecycle::AwaitingFirstPeer {
            initial_generation: reader.generation().expect("generation")
        }
    );
    assert!(
        !reader.peer_closed().expect("peer close"),
        "an unattached peer is not a closed peer"
    );
}

fn install_generation_callbacks() {
    INSTALL.call_once(|| {
        selium_memory::install_generation_callbacks(register_generation, wake_generation);
    });
}

fn install_heap_provider() {
    drop(selium_memory::set_region_provider(Box::new(
        selium_memory::HeapRegionProvider::new(),
    )));
}

fn noop_waker() -> Waker {
    futures::task::noop_waker()
}

// scenario: peer-lifecycle/initial-peer-absence-is-not-closure/ordinary-channel-retains-its-membership-policy
#[test]
fn ordinary_channel_retains_immediate_eof() {
    install_heap_provider();
    let channel = park_channel(64);
    let mut reader = channel.blocking_reader().expect("legacy reader");

    let (result, filled) = poll_read_once(&mut reader, &noop_waker());
    assert!(matches!(result, Poll::Ready(Ok(()))), "legacy EOF");
    assert!(filled.is_empty(), "no data before EOF");
    assert!(reader.peer_closed().expect("peer close"));
}

fn park_channel(capacity: u64) -> Channel {
    Channel::create_with_backpressure(
        capacity,
        ChannelBackpressure::Park,
        ResourceKind::SharedMemory,
    )
    .expect("create channel")
}
// scenario: channel-wake-wait/wakeable-channel-reads/reader-observes-disconnect-without-spinning

// scenario: peer-lifecycle/peer-attachment-is-observable/peer-attaches-and-closes-before-the-first-read
#[test]
fn peer_attaches_and_closes_before_the_first_poll() {
    install_heap_provider();
    let channel = park_channel(64);
    let mut reader = channel
        .blocking_reader_awaiting_peer()
        .expect("awaiting reader");
    let writer = channel.blocking_writer().expect("peer attaches");
    drop(writer);

    let (result, filled) = poll_read_once(&mut reader, &noop_waker());
    assert!(
        matches!(result, Poll::Ready(Ok(()))),
        "a peer that came and went is closure, not initial absence"
    );
    assert!(filled.is_empty());
    assert!(reader.peer_closed().expect("peer close"));
}

/// Polls the reader once, returning the poll result and whatever bytes it
/// produced.
fn poll_read_once(
    reader: &mut BlockingReader,
    waker: &Waker,
) -> (Poll<std::io::Result<()>>, Vec<u8>) {
    let mut context = Context::from_waker(waker);
    let mut storage = [0u8; 64];
    let mut buf = ReadBuf::new(&mut storage);
    let result = Pin::new(reader).poll_read(&mut context, &mut buf);
    let filled = buf.filled().to_vec();
    (result, filled)
}

fn register_generation(region_id: u64, generation: u64, waker: &Waker) {
    let mut waits = WAITS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    waits.push((region_id, generation, waker.clone()));
}

fn wake_generation(region_id: u64, new_generation: u64) {
    let mut waits = WAITS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut remaining = Vec::new();
    for (rid, generation, waker) in waits.drain(..) {
        if rid == region_id && generation < new_generation {
            waker.wake();
        } else {
            remaining.push((rid, generation, waker));
        }
    }
    *waits = remaining;
}

fn write_frame(channel: &Channel, payload: &[u8]) {
    let required = FrameHeader::ENCODED_SIZE as u64 + payload.len() as u64;
    let position = channel.ring().reserve(required).expect("reserve");
    channel
        .ring()
        .write_frame(position, payload, 0, 0)
        .expect("write frame");
}

// --- Spec traceability (scripts/check-traceability.sh) -------------------
// These tests are the evidence for the `peer-lifecycle` and
// `channel-wake-wait` writer-attachment scenarios. The ids resolve against
// `openspec/specs` once this change is archived.
