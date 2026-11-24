//! Smoke test exercising `selium-wire` patterns over a tokio duplex mock transport.

use std::{
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use selium_memory::FrameHeader;
use selium_service::FlatMsg;
use selium_wire::{
    FramedRead, FramedWrite, MessageTransport, Publisher, Subscriber, error::Result,
};
use tokio::io::{AsyncRead, AsyncWrite, DuplexStream, ReadBuf};

struct MockTransport(DuplexStream);

#[derive(Default)]
struct MockArbiter(AtomicU64);

impl selium_wire::TableWriterArbiter for MockArbiter {
    fn claim_table_writer(&self, claim: u64) -> Result<bool> {
        Ok(self
            .0
            .compare_exchange(0, claim, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok())
    }

    fn release_table_writer(&self, claim: u64) -> Result<()> {
        if self
            .0
            .compare_exchange(claim, 0, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            Ok(())
        } else {
            Err(selium_wire::Error::TableAuthorityConflict {
                topic: "mock authority changed during release".to_string(),
            })
        }
    }
}

thread_local! {
    static MOCK_ARBITER: Arc<MockArbiter> = Arc::new(MockArbiter::default());
}

#[derive(Debug, Clone, PartialEq)]
struct Greeting(String);

impl AsyncRead for MockTransport {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        Pin::new(&mut self.0).poll_read(cx, buf)
    }
}

impl AsyncWrite for MockTransport {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        Pin::new(&mut self.0).poll_write(cx, buf)
    }

    fn poll_flush(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        Pin::new(&mut self.0).poll_flush(cx)
    }

    fn poll_shutdown(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        Pin::new(&mut self.0).poll_shutdown(cx)
    }
}

impl MessageTransport for MockTransport {
    type Error = std::io::Error;

    fn poll_ready(
        self: Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<bool>> {
        std::task::Poll::Ready(Ok(true))
    }

    fn poll_peer_closed(
        self: Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<bool>> {
        std::task::Poll::Ready(Ok(false))
    }

    fn generation(&self) -> Result<u64> {
        Ok(0)
    }

    fn table_writer_arbiter(&self) -> Option<Arc<dyn selium_wire::TableWriterArbiter>> {
        MOCK_ARBITER
            .with(|arbiter| Some(Arc::clone(arbiter) as Arc<dyn selium_wire::TableWriterArbiter>))
    }
}

impl FlatMsg for Greeting {
    fn encode(value: &Self) -> Vec<u8> {
        value.0.clone().into_bytes()
    }

    fn decode(bytes: &[u8]) -> std::result::Result<Self, flatbuffers::InvalidFlatbuffer> {
        Ok(Self(String::from_utf8(bytes.to_vec()).map_err(
            |_error| flatbuffers::InvalidFlatbuffer::ApparentSizeTooLarge,
        )?))
    }
}

#[test]
fn frame_header_round_trip() {
    let header = FrameHeader {
        len: 5,
        tag: 99,
        flags: FrameHeader::FLAG_READY,
    };
    let encoded = header.encode();
    let decoded = FrameHeader::decode(&encoded).unwrap();
    assert_eq!(decoded, header);
}
// scenario: selium-guest-macros/wire-parameter-on-schema-macro/wire-parameter-triggers-wire-type-generation
// scenario: selium-guest-macros/generic-parameter-auto-detection-for-byte-fields/direct-generic-field-detection
// scenario: selium-guest-macros/generic-parameter-auto-detection-for-byte-fields/concrete-type-field-is-not-detected-as-generic
// scenario: selium-guest-macros/bridge-encode-impl-for-generic-types/encode-serialises-generics-to-bytes
// scenario: selium-guest-macros/bridge-encode-impl-for-generic-types/encode-handles-none-option-scalars
// scenario: selium-guest-macros/bridge-decode-impl-for-generic-types/decode-reconstructs-generics-from-bytes
// scenario: selium-guest-macros/bridge-decode-impl-for-generic-types/decode-handles-zero-sentinel-as-none

/// 1.4: `LiveTable::sync_async` parks on the transport's read waker for
/// remote writes instead of spinning, then applies the mutation to the local
/// materialised view.
// scenario: selium-guest/live-table-mutation-has-one-authority/competing-writers-attempt-cas
// scenario: selium-guest/live-table-mutation-has-one-authority/api-lacks-an-arbiter
#[tokio::test]
async fn live_table_write_authority_is_exclusive_and_cas_conflicts_are_deterministic() {
    use selium_wire::{LiveTable, TableWriterAuthority, error::Error};

    let authority = TableWriterAuthority::claim("mock.cas-authority").expect("claim");

    // Loopback pair: the table's publisher feeds its own subscriber.
    let (write, read) = tokio::io::duplex(64 * 1024);
    let publisher = Publisher::new(FramedWrite::new(MockTransport(write)));
    let subscriber = Subscriber::new(FramedRead::new(MockTransport(read)), None);
    let table: LiveTable<String, u64, _> =
        LiveTable::new(authority, publisher, subscriber).expect("table");

    let (other_write, other_read) = tokio::io::duplex(1024);
    let competing: selium_wire::Result<LiveTable<String, u64, _>> = LiveTable::new(
        TableWriterAuthority::claim("mock.cas-authority-alias").expect("alias claim"),
        Publisher::new(FramedWrite::new(MockTransport(other_write))),
        Subscriber::new(FramedRead::new(MockTransport(other_read)), None),
    );
    assert!(
        matches!(competing, Err(Error::TableAuthorityConflict { .. })),
        "a competing writer on the same arbiter must fail loudly"
    );

    table.set("k".to_string(), 1).expect("set");
    assert!(matches!(
        table.compare_and_set("k".to_string(), 0, 2),
        Err(Error::CasConflict { .. })
    ));
    let version = table
        .compare_and_set("k".to_string(), 1, 2)
        .expect("matching version applies");
    assert!(matches!(
        table.compare_and_set("k".to_string(), 1, 3),
        Err(Error::CasConflict { .. })
    ));
    assert!(version >= 2);
    assert_eq!(table.get(&"k".to_string()).expect("get"), Some(2));
}
// scenario: selium-guest-macros/generic-parameter-auto-detection-for-byte-fields/option-wrapped-generic-field-detection

#[tokio::test]
async fn live_table_sync_async_reads_remote_writes_without_spinning() {
    use selium_wire::tables::LiveTableMessage;
    use std::{sync::Arc, time::Duration};

    // The table's own publisher talks over an unrelated pair (the test drives
    // remote writes only); the table's subscriber reads the remote pair.
    let (own_write, _own_read) = tokio::io::duplex(64);
    let (remote_write, remote_read) = tokio::io::duplex(4096);

    let publisher = Publisher::new(FramedWrite::new(MockTransport(own_write)));
    let subscriber = Subscriber::new(FramedRead::new(MockTransport(remote_read)), None);
    let table = selium_wire::LiveTable::new(
        selium_wire::TableWriterAuthority::claim("mock.remote-sync").expect("authority"),
        publisher,
        subscriber,
    )
    .expect("live table over mock transports");

    // A remote publisher writes one mutation after a released gate.
    let notify = Arc::new(tokio::sync::Notify::new());
    let gate = notify.clone();
    let mut remote = Publisher::new(FramedWrite::new(MockTransport(remote_write)));
    let remote_task = tokio::spawn(async move {
        gate.notified().await;
        remote
            .publish(&LiveTableMessage {
                mutation_id: 1,
                key: "alpha".to_string(),
                value: Some(10u64),
                expected_version: None,
            })
            .expect("remote publish");
    });

    // No remote write is available yet: sync_async must park, not return.
    assert!(
        tokio::time::timeout(Duration::from_millis(25), table.sync_async())
            .await
            .is_err(),
        "sync_async must park until a remote write arrives"
    );

    notify.notify_one();

    tokio::time::timeout(Duration::from_secs(5), table.sync_async())
        .await
        .expect("sync_async completes after the remote write")
        .expect("sync_async applies the remote write");

    assert_eq!(table.get(&"alpha".to_string()).expect("get"), Some(10u64));
    remote_task.await.expect("remote task");
}

#[test]
fn publisher_sink_start_send_over_mock_transport() {
    let (client, server) = tokio::io::duplex(1024);

    let mut publisher: Publisher<Greeting, MockTransport> =
        Publisher::new(FramedWrite::new(MockTransport(client)));

    use futures::SinkExt;
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        publisher
            .send(Greeting("sink".to_string()))
            .await
            .expect("sink send");
    });

    let mut subscriber: Subscriber<Greeting, MockTransport> =
        Subscriber::new(FramedRead::new(MockTransport(server)), None);
    let (received, _) = subscriber.read_with_tag().unwrap();
    assert_eq!(received, Greeting("sink".to_string()));
}

// scenario: selium-guest/messaging-pattern-layer/prototype-local-pattern-composition
#[tokio::test]
async fn pubsub_round_trip_over_mock_transport() {
    let (client, server) = tokio::io::duplex(1024);

    let mut publisher: Publisher<Greeting, MockTransport> =
        Publisher::new(FramedWrite::new(MockTransport(client)));
    publisher.set_writer_id(7);

    let mut subscriber: Subscriber<Greeting, MockTransport> =
        Subscriber::new(FramedRead::new(MockTransport(server)), None);

    publisher
        .publish(&Greeting("hello".to_string()))
        .expect("publish greeting");

    let (received, writer_id) = subscriber.read_with_tag().expect("read greeting");
    assert_eq!(received, Greeting("hello".to_string()));
    assert_eq!(writer_id, 7);
}

// scenario: transport-abstraction/waker-honest-frame-readiness/socket-transport-parks-through-the-codec
// scenario: framed-io/waker-honest-async-read-path/native-read-parks-instead-of-spinning
/// 1.2: a `Subscriber` awaited for the next message must park on the
/// transport's read waker until a frame arrives — it must not complete early,
/// and (unlike a generation/yield fallback) it must observe a frame that
/// arrives after the await begins.
#[tokio::test]
async fn subscriber_parks_on_socket_until_frame_arrives() {
    use futures::StreamExt;
    use std::{sync::Arc, time::Duration};

    let (client, server) = tokio::io::duplex(4096);
    let mut publisher = Publisher::new(FramedWrite::new(MockTransport(client)));
    let mut subscriber = Subscriber::new(FramedRead::new(MockTransport(server)), None);

    // The write is gated behind a notify so delivery happens strictly after
    // the subscriber has begun awaiting.
    let notify = Arc::new(tokio::sync::Notify::new());
    let gate = notify.clone();
    let writer = tokio::spawn(async move {
        gate.notified().await;
        publisher
            .publish(&Greeting("hello".to_string()))
            .expect("publish");
    });

    // Nothing is available yet: awaiting must park, not complete.
    assert!(
        tokio::time::timeout(Duration::from_millis(25), subscriber.next())
            .await
            .is_err(),
        "subscriber must park until a frame arrives"
    );

    notify.notify_one();

    let item = tokio::time::timeout(Duration::from_secs(5), subscriber.next())
        .await
        .expect("frame arrives after the write is released");
    assert_eq!(
        item.map(|message| message.expect("decode")),
        Some(Greeting("hello".to_string()))
    );

    writer.await.expect("writer task");
}

/// Transport whose reads fail with an overwrite error, used to prove the
/// framed reader surfaces it instead of reporting an empty read.
struct OverwriteTransport;

impl AsyncRead for OverwriteTransport {
    fn poll_read(
        self: Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
        _buf: &mut ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Err(std::io::Error::other(
            "transport overwrote unread bytes",
        )))
    }
}

impl AsyncWrite for OverwriteTransport {
    fn poll_write(
        self: Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        std::task::Poll::Ready(Ok(buf.len()))
    }

    fn poll_flush(
        self: Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn poll_shutdown(
        self: Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }
}

impl MessageTransport for OverwriteTransport {
    type Error = std::io::Error;

    fn poll_ready(
        self: Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<bool>> {
        std::task::Poll::Ready(Ok(true))
    }

    fn poll_peer_closed(
        self: Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<bool>> {
        std::task::Poll::Ready(Ok(false))
    }

    fn generation(&self) -> Result<u64> {
        Ok(0)
    }
}

/// Transport with a fixed generation counter and no readable frames.
struct GenerationTransport(u64);

impl AsyncRead for GenerationTransport {
    fn poll_read(
        self: Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
        _buf: &mut ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Pending
    }
}

impl AsyncWrite for GenerationTransport {
    fn poll_write(
        self: Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        std::task::Poll::Ready(Ok(buf.len()))
    }

    fn poll_flush(
        self: Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn poll_shutdown(
        self: Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }
}

impl MessageTransport for GenerationTransport {
    type Error = std::io::Error;

    fn poll_ready(
        self: Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<bool>> {
        std::task::Poll::Ready(Ok(false))
    }

    fn poll_peer_closed(
        self: Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<bool>> {
        std::task::Poll::Ready(Ok(false))
    }

    fn generation(&self) -> Result<u64> {
        Ok(self.0)
    }
}

// scenario: framed-io/framedread-type/underlying-transport-returns-overwrite-error
#[test]
fn framed_read_surfaces_a_transport_overwrite_error() {
    let mut read = FramedRead::<OverwriteTransport>::new(OverwriteTransport);
    let error = read
        .read_frame()
        .expect_err("an overwrite is an error, not an empty read");
    assert!(
        error.to_string().contains("overwrote unread bytes"),
        "the transport's overwrite diagnosis must reach the caller: {error}"
    );
}

// scenario: framed-io/framedread-exposes-generation-counter/read-generation-counter
#[test]
fn framed_read_reports_the_transport_generation() {
    let read = FramedRead::<GenerationTransport>::new(GenerationTransport(41));
    assert_eq!(
        read.generation().expect("generation counter"),
        41,
        "the framed reader exposes the transport's generation"
    );
}

// scenario: transport-abstraction/messagetransport-trait/transport-signals-empty
#[test]
fn framed_read_reports_an_empty_transport_without_spinning() {
    let mut read = FramedRead::<GenerationTransport>::new(GenerationTransport(0));
    assert!(
        !read.poll_ready().expect("readiness probe"),
        "an empty transport signals `no frame ready`, not an error"
    );
}
