//! Smoke test exercising `selium-wire` patterns over a tokio duplex mock transport.

use std::pin::Pin;

use selium_memory::FrameHeader;
use selium_service::FlatMsg;
use selium_wire::{
    FramedRead, FramedWrite, MessageTransport, Publisher, Subscriber, error::Result,
};
use tokio::io::{AsyncRead, AsyncWrite, DuplexStream, ReadBuf};

struct MockTransport(DuplexStream);

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

/// 1.4: `LiveTable::sync_async` parks on the transport's read waker for
/// remote writes instead of spinning, then applies the mutation to the local
/// materialised view.
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
    let table = selium_wire::LiveTable::new(publisher, subscriber)
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
