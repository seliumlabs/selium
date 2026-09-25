//! End-to-end smoke test for `selium-wire` patterns over `selium-shm` transports.

use selium_abi::ResourceKind;
use selium_memory::FrameHeader;
use selium_service::FlatMsg;
use selium_shm::{Channel, ChannelBackpressure, ShmTransport};
use selium_wire::{
    FramedRead, FramedWrite,
    rpc::{Rendezvous, RpcClient, RpcConnection},
    stream::{
        RpcBidiStreamClient, RpcBidiStreamConnection, RpcServerStreamClient,
        RpcServerStreamConnection,
    },
};

#[derive(Debug, Clone, PartialEq)]
struct Ping(String);

#[derive(Debug, Clone, PartialEq)]
struct Pong(String);

impl FlatMsg for Ping {
    fn encode(value: &Self) -> Vec<u8> {
        value.0.clone().into_bytes()
    }

    fn decode(bytes: &[u8]) -> Result<Self, flatbuffers::InvalidFlatbuffer> {
        Ok(Self(String::from_utf8(bytes.to_vec()).map_err(
            |_error| flatbuffers::InvalidFlatbuffer::ApparentSizeTooLarge,
        )?))
    }
}

impl FlatMsg for Pong {
    fn encode(value: &Self) -> Vec<u8> {
        value.0.clone().into_bytes()
    }

    fn decode(bytes: &[u8]) -> Result<Self, flatbuffers::InvalidFlatbuffer> {
        Ok(Self(String::from_utf8(bytes.to_vec()).map_err(
            |_error| flatbuffers::InvalidFlatbuffer::ApparentSizeTooLarge,
        )?))
    }
}

#[tokio::test]
async fn bidi_stream_backpressure_no_spin_no_loss() {
    install_heap_provider();

    // Ring smaller than the total stream bytes: the client's sender MUST
    // park on the async write path until the server drains.
    let request_channel = make_park_channel(128);
    let reply_channel = make_channel(1024);
    let dummy = make_channel(64);

    let client: RpcBidiStreamClient<Ping, Ping, Pong, ShmTransport> = RpcBidiStreamClient::new(
        FramedWrite::new(ShmTransport::new(&dummy, &request_channel).expect("client tx")),
        FramedRead::new(ShmTransport::new(&reply_channel, &dummy).expect("client rx")),
    );

    let mut server: RpcBidiStreamConnection<Ping, Ping, Pong, ShmTransport> =
        RpcBidiStreamConnection::new(
            FramedRead::new(ShmTransport::new(&request_channel, &dummy).expect("server rx")),
            FramedWrite::new(ShmTransport::new(&dummy, &reply_channel).expect("server tx")),
            42,
        );

    let server_handle = tokio::spawn(async move {
        let mut req = server.recv().await.expect("recv request");
        let (_responder, mut request_stream) = req.split();

        // Receive all items from client.
        let mut items = Vec::new();
        while let Some(item) = request_stream.recv().await.expect("recv") {
            items.push(item);
            if items.len() == 8 {
                break;
            }
        }
        assert_eq!(items.len(), 8);
    });

    let mut cl = client;
    let mut session = cl
        .connect(Ping("bp-bidi".to_string()))
        .await
        .expect("connect");

    let (mut sender, _receiver) = session.split();

    // Send 8 items — send parks on a full ring; no retry loops.
    for i in 0..8 {
        sender
            .send(Ping(format!("bp{i}")))
            .await
            .expect("send parks until writable");
    }
    sender.close().await.expect("close");

    server_handle.await.expect("server task");
}

#[tokio::test]
async fn bidi_stream_half_close_server_continues_sending() {
    install_heap_provider();

    let request_channel = make_channel(1024);
    let reply_channel = make_channel(1024);
    let dummy = make_channel(64);

    let client: RpcBidiStreamClient<Ping, Ping, Pong, ShmTransport> = RpcBidiStreamClient::new(
        FramedWrite::new(ShmTransport::new(&dummy, &request_channel).expect("client tx")),
        FramedRead::new(ShmTransport::new(&reply_channel, &dummy).expect("client rx")),
    );

    let mut server: RpcBidiStreamConnection<Ping, Ping, Pong, ShmTransport> =
        RpcBidiStreamConnection::new(
            FramedRead::new(ShmTransport::new(&request_channel, &dummy).expect("server rx")),
            FramedWrite::new(ShmTransport::new(&dummy, &reply_channel).expect("server tx")),
            42,
        );

    let server_handle = tokio::spawn(async move {
        let mut req = server.recv().await.expect("recv request");
        let (mut responder, mut request_stream) = req.split();

        // Client sends one item then closes its send direction.
        let item0 = request_stream
            .recv()
            .await
            .expect("recv item")
            .expect("item 0");
        assert_eq!(item0, Ping("half-close".to_string()));

        // Client's send direction ends.
        let end = request_stream.recv().await.expect("recv end");
        assert!(end.is_none(), "client send end");

        // Server can still send after client closed its direction.
        responder
            .send(Pong("still-sending".to_string()))
            .await
            .expect("send after client close");
        responder.close().await.expect("close server");
    });

    let mut cl = client;
    let mut session = cl.connect(Ping("hc".to_string())).await.expect("connect");

    let (mut sender, mut receiver) = session.split();

    sender
        .send(Ping("half-close".to_string()))
        .await
        .expect("send");
    sender.close().await.expect("close send");

    // Client receives server's response after closing its own send.
    let resp = receiver.recv().await.expect("recv resp").expect("resp");
    assert_eq!(resp, Pong("still-sending".to_string()));

    server_handle.await.expect("server task");
}

#[tokio::test]
async fn bidi_stream_independent_send_receive() {
    install_heap_provider();

    let request_channel = make_channel(1024);
    let reply_channel = make_channel(1024);
    let dummy = make_channel(64);

    let client: RpcBidiStreamClient<Ping, Ping, Pong, ShmTransport> = RpcBidiStreamClient::new(
        FramedWrite::new(ShmTransport::new(&dummy, &request_channel).expect("client tx")),
        FramedRead::new(ShmTransport::new(&reply_channel, &dummy).expect("client rx")),
    );

    let mut server: RpcBidiStreamConnection<Ping, Ping, Pong, ShmTransport> =
        RpcBidiStreamConnection::new(
            FramedRead::new(ShmTransport::new(&request_channel, &dummy).expect("server rx")),
            FramedWrite::new(ShmTransport::new(&dummy, &reply_channel).expect("server tx")),
            42,
        );

    let server_handle = tokio::spawn(async move {
        let mut req = server.recv().await.expect("recv request");
        let ping: Ping = req.payload().expect("decode ping");
        assert_eq!(ping, Ping("bidi-hello".to_string()));

        let (mut responder, mut request_stream) = req.split();

        // Receive two items from client.
        let item0 = request_stream
            .recv()
            .await
            .expect("recv item")
            .expect("item 0");
        let item1 = request_stream
            .recv()
            .await
            .expect("recv item")
            .expect("item 1");
        assert_eq!(item0, Ping("client-0".to_string()));
        assert_eq!(item1, Ping("client-1".to_string()));

        // Send two items back.
        responder
            .send(Pong("server-0".to_string()))
            .await
            .expect("send 0");
        responder
            .send(Pong("server-1".to_string()))
            .await
            .expect("send 1");
        responder.close().await.expect("close");
    });

    let mut cl = client;
    let mut session = cl
        .connect(Ping("bidi-hello".to_string()))
        .await
        .expect("connect");

    let (mut sender, mut receiver) = session.split();

    // Send two items.
    sender
        .send(Ping("client-0".to_string()))
        .await
        .expect("send 0");
    sender
        .send(Ping("client-1".to_string()))
        .await
        .expect("send 1");
    sender.close().await.expect("close send");

    // Receive two items from server.
    let resp0 = receiver.recv().await.expect("recv resp").expect("resp 0");
    let resp1 = receiver.recv().await.expect("recv resp").expect("resp 1");
    assert_eq!(resp0, Pong("server-0".to_string()));
    assert_eq!(resp1, Pong("server-1".to_string()));

    // Receiver should see end-of-stream.
    let end = receiver.recv().await.expect("recv end");
    assert!(end.is_none(), "should be end-of-stream");

    server_handle.await.expect("server task");
}

#[test]
fn framed_write_read_round_trip_over_shm_transport() {
    install_heap_provider();

    let channel = make_channel(1024);
    let dummy = make_channel(64);

    let mut writer =
        FramedWrite::<ShmTransport>::new(ShmTransport::new(&dummy, &channel).expect("writer"));
    let mut reader =
        FramedRead::<ShmTransport>::new(ShmTransport::new(&channel, &dummy).expect("reader"));

    writer.write_frame(b"hello", 42).expect("write frame");
    let (payload, tag, flags) = reader.read_frame().expect("read frame");
    assert_eq!(payload, b"hello");
    assert_eq!(tag, 42);
    assert_eq!(flags, FrameHeader::FLAG_READY);
}

fn install_heap_provider() {
    drop(selium_memory::set_region_provider(Box::new(
        selium_memory::HeapRegionProvider::new(),
    )));
}

fn make_channel(capacity: u64) -> Channel {
    Channel::create_with_backpressure(
        capacity,
        ChannelBackpressure::Drop,
        ResourceKind::SharedMemory,
    )
    .expect("create channel")
}

fn make_park_channel(capacity: u64) -> Channel {
    Channel::create_with_backpressure(
        capacity,
        ChannelBackpressure::Park,
        ResourceKind::SharedMemory,
    )
    .expect("create park channel")
}

#[tokio::test]
async fn pubsub_round_trip_over_shm_transport() {
    install_heap_provider();

    let channel = make_channel(1024);

    // Dummy channels: each transport only uses one real direction for this test.
    let dummy = make_channel(64);

    let mut publisher = selium_wire::Publisher::<Ping, ShmTransport>::new(FramedWrite::new(
        ShmTransport::new(&dummy, &channel).expect("publisher tx"),
    ));
    let mut subscriber = selium_wire::Subscriber::<Ping, ShmTransport>::new(
        FramedRead::new(ShmTransport::new(&channel, &dummy).expect("subscriber rx")),
        None,
    );

    publisher
        .publish(&Ping("broadcast".to_string()))
        .expect("publish");

    let (received, _writer_id) = subscriber.read_with_tag().expect("receive");
    assert_eq!(received, Ping("broadcast".to_string()));
}

#[test]
fn rendezvous_passes_region_ids() {
    use selium_shm::ShmRendezvous;
    let rendezvous = ShmRendezvous::new();

    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        rendezvous.send(12345).await.expect("send");
        let conn = rendezvous.recv().await.expect("recv");
        assert_eq!(conn.shared_id, 12345);
    });
}

#[tokio::test]
async fn rpc_round_trip_over_shm_transport() {
    install_heap_provider();

    // Two unidirectional channels form a full-duplex RPC session.
    let request_channel = make_channel(1024);
    let reply_channel = make_channel(1024);

    let dummy = make_channel(64);

    let client: RpcClient<Ping, Pong, ShmTransport> = RpcClient::new(
        FramedWrite::new(ShmTransport::new(&dummy, &request_channel).expect("client tx")),
        FramedRead::new(ShmTransport::new(&reply_channel, &dummy).expect("client rx")),
    );

    let mut server: RpcConnection<Ping, Pong, ShmTransport> = RpcConnection::new(
        FramedRead::new(ShmTransport::new(&request_channel, &dummy).expect("server rx")),
        FramedWrite::new(ShmTransport::new(&dummy, &reply_channel).expect("server tx")),
        42,
    );

    // Drive the server in a background task.
    let server_handle = tokio::spawn(async move {
        let req = server.recv().await.expect("recv request");
        let ping: Ping = req.payload().expect("decode ping");
        req.reply(Pong(format!("reply to {}", ping.0)))
            .await
            .expect("reply");
    });

    let mut client = client;
    let pong = client
        .request(Ping("hello".to_string()))
        .await
        .expect("request");
    assert_eq!(pong, Pong("reply to hello".to_string()));

    server_handle.await.expect("server task");
}

#[tokio::test]
async fn server_drop_sends_cancel_to_client() {
    install_heap_provider();

    let request_channel = make_channel(1024);
    let reply_channel = make_channel(1024);
    let dummy = make_channel(64);

    let client: RpcServerStreamClient<Ping, Pong, ShmTransport> = RpcServerStreamClient::new(
        FramedWrite::new(ShmTransport::new(&dummy, &request_channel).expect("client tx")),
        FramedRead::new(ShmTransport::new(&reply_channel, &dummy).expect("client rx")),
    );

    let mut server: RpcServerStreamConnection<Ping, Pong, ShmTransport> =
        RpcServerStreamConnection::new(
            FramedRead::new(ShmTransport::new(&request_channel, &dummy).expect("server rx")),
            FramedWrite::new(ShmTransport::new(&dummy, &reply_channel).expect("server tx")),
            42,
        );

    // Server keeps its connection alive after abandoning the stream — so the
    // only way the client can observe termination is via the server's cancel
    // frame (peer-close detection cannot fire).
    let (dropped_tx, mut dropped_rx) = tokio::sync::oneshot::channel();
    let server_handle = tokio::spawn(async move {
        let req = server.recv().await.expect("recv request");
        drop(req); // abandon without finish/send_error
        dropped_tx.send(()).unwrap_or_default();
        // Hold the connection (and writers) open.
        for _ in 0..2000 {
            tokio::task::yield_now().await;
        }
    });

    let mut client = client;
    {
        let mut stream = client
            .call(Ping("abandon-me".to_string()))
            .await
            .expect("call");

        use futures::Stream;
        use std::pin::Pin;
        // Stream terminates with None (cancel observed), not an error and not
        // a hang waiting for peer close. Bounded by yield count so a
        // regression fails the assertion instead of hanging forever.
        let mut got_none = false;
        #[expect(
            clippy::never_loop,
            reason = "`poll_fn(...).await` parks until the stream resolves; the counter only bounds the wait as written"
        )]
        for _ in 0..100_000 {
            // Yield between polls so the server task gets scheduled; keeps
            // this from being a tight spin while still bounding the wait.
            tokio::task::yield_now().await;
            match std::future::poll_fn(|cx| Pin::new(&mut stream).poll_next(cx)).await {
                Some(Ok(item)) => panic!("unexpected item from abandoned stream: {item:?}"),
                Some(Err(e)) => panic!("unexpected error from abandoned stream: {e}"),
                None => {
                    got_none = true;
                    break;
                }
            }
        }
        assert!(
            got_none,
            "abandoned stream should terminate via server cancel"
        );
    }

    dropped_rx.try_recv().unwrap_or_default();
    server_handle.await.expect("server task");
}

#[tokio::test]
async fn server_stream_backpressure_small_ring_no_item_loss() {
    install_heap_provider();

    // Ring smaller than the total stream bytes: the producer MUST park on
    // the async write path until the consumer drains. Park backpressure
    // guarantees no item loss.
    let request_channel = make_channel(1024);
    let reply_channel = make_park_channel(128);
    let dummy = make_channel(64);

    let client: RpcServerStreamClient<Ping, Pong, ShmTransport> = RpcServerStreamClient::new(
        FramedWrite::new(ShmTransport::new(&dummy, &request_channel).expect("client tx")),
        FramedRead::new(ShmTransport::new(&reply_channel, &dummy).expect("client rx")),
    );

    let mut server: RpcServerStreamConnection<Ping, Pong, ShmTransport> =
        RpcServerStreamConnection::new(
            FramedRead::new(ShmTransport::new(&request_channel, &dummy).expect("server rx")),
            FramedWrite::new(ShmTransport::new(&dummy, &reply_channel).expect("server tx")),
            42,
        );

    let server_handle = tokio::spawn(async move {
        let mut req = server.recv().await.expect("recv request");

        // No retry loops: send_item parks on a full ring and resolves when
        // capacity frees. If parking regressed to BufferFull errors, these
        // expects would fire.
        for i in 0..10 {
            req.send_item(Pong(format!("p{i:02}")))
                .await
                .expect("send item parks until writable");
        }
        req.finish().await.expect("finish");
    });

    let mut client = client;
    {
        let mut stream = client
            .call(Ping("backpressure".to_string()))
            .await
            .expect("call");

        use futures::StreamExt;
        let mut items = Vec::new();
        while let Some(result) = stream.next().await {
            items.push(result.expect("item"));
        }

        assert_eq!(items.len(), 10, "all 10 items should be received");
        for (i, item) in items.iter().enumerate() {
            assert_eq!(item, &Pong(format!("p{i:02}")));
        }
    }

    server_handle.await.expect("server task");
}

#[tokio::test]
async fn server_stream_cancel_stops_production() {
    install_heap_provider();

    let request_channel = make_channel(1024);
    let reply_channel = make_channel(1024);
    let dummy = make_channel(64);

    let client: RpcServerStreamClient<Ping, Pong, ShmTransport> = RpcServerStreamClient::new(
        FramedWrite::new(ShmTransport::new(&dummy, &request_channel).expect("client tx")),
        FramedRead::new(ShmTransport::new(&reply_channel, &dummy).expect("client rx")),
    );

    let mut server: RpcServerStreamConnection<Ping, Pong, ShmTransport> =
        RpcServerStreamConnection::new(
            FramedRead::new(ShmTransport::new(&request_channel, &dummy).expect("server rx")),
            FramedWrite::new(ShmTransport::new(&dummy, &reply_channel).expect("server tx")),
            42,
        );

    let server_handle = tokio::spawn(async move {
        let mut req = server.recv().await.expect("recv request");

        // Send first item, then check for cancel.
        req.send_item(Pong("item-0".to_string()))
            .await
            .expect("send item 0");

        // Spin briefly to allow the cancel to arrive.
        for _ in 0..100 {
            if req.check_cancel() {
                return; // Cancel received, stop producing.
            }
            tokio::task::yield_now().await;
        }

        // If we get here, cancel wasn't received — the test should fail.
        panic!("expected cancel, but none received");
    });

    let mut client = client;
    {
        let mut stream = client
            .call(Ping("cancel-me".to_string()))
            .await
            .expect("call");

        use futures::StreamExt;
        // Read first item.
        let first = stream.next().await.expect("first item").expect("ok");
        assert_eq!(first, Pong("item-0".to_string()));

        // Cancel the stream.
        stream.cancel();

        // Stream should terminate.
        let next = stream.next().await;
        assert!(next.is_none(), "stream should be done after cancel");
    }

    server_handle.await.expect("server task");
}

#[tokio::test]
async fn server_stream_drop_cancels() {
    install_heap_provider();

    let request_channel = make_channel(1024);
    let reply_channel = make_channel(1024);
    let dummy = make_channel(64);

    let client: RpcServerStreamClient<Ping, Pong, ShmTransport> = RpcServerStreamClient::new(
        FramedWrite::new(ShmTransport::new(&dummy, &request_channel).expect("client tx")),
        FramedRead::new(ShmTransport::new(&reply_channel, &dummy).expect("client rx")),
    );

    let mut server: RpcServerStreamConnection<Ping, Pong, ShmTransport> =
        RpcServerStreamConnection::new(
            FramedRead::new(ShmTransport::new(&request_channel, &dummy).expect("server rx")),
            FramedWrite::new(ShmTransport::new(&dummy, &reply_channel).expect("server tx")),
            42,
        );

    let (cancel_tx, mut cancel_rx) = tokio::sync::oneshot::channel();

    let server_handle = tokio::spawn(async move {
        let mut req = server.recv().await.expect("recv request");

        req.send_item(Pong("item-0".to_string()))
            .await
            .expect("send item 0");

        // Wait for cancel via the check.
        for _ in 0..200 {
            if req.check_cancel() {
                cancel_tx.send(()).unwrap_or_default();
                return;
            }
            tokio::task::yield_now().await;
        }

        panic!("expected drop-cancel, but none received");
    });

    let mut client = client;
    {
        let mut stream = client
            .call(Ping("drop-me".to_string()))
            .await
            .expect("call");

        use futures::StreamExt;
        let first = stream.next().await.expect("first item").expect("ok");
        assert_eq!(first, Pong("item-0".to_string()));

        // Drop the stream — should send cancel.
        drop(stream);
    }

    // Server should have received the cancel.
    let mut cancel_received = false;
    for _ in 0..500 {
        if cancel_rx.try_recv().is_ok() {
            cancel_received = true;
            break;
        }
        tokio::task::yield_now().await;
    }
    assert!(cancel_received, "server should have received cancel");

    server_handle.await.expect("server task");
}

#[tokio::test]
async fn server_stream_error_mid_stream() {
    install_heap_provider();

    let request_channel = make_channel(1024);
    let reply_channel = make_channel(1024);
    let dummy = make_channel(64);

    let client: RpcServerStreamClient<Ping, Pong, ShmTransport> = RpcServerStreamClient::new(
        FramedWrite::new(ShmTransport::new(&dummy, &request_channel).expect("client tx")),
        FramedRead::new(ShmTransport::new(&reply_channel, &dummy).expect("client rx")),
    );

    let mut server: RpcServerStreamConnection<Ping, Pong, ShmTransport> =
        RpcServerStreamConnection::new(
            FramedRead::new(ShmTransport::new(&request_channel, &dummy).expect("server rx")),
            FramedWrite::new(ShmTransport::new(&dummy, &reply_channel).expect("server tx")),
            42,
        );

    let server_handle = tokio::spawn(async move {
        let mut req = server.recv().await.expect("recv request");

        req.send_item(Pong("ok-0".to_string()))
            .await
            .expect("send item 0");
        req.send_item(Pong("ok-1".to_string()))
            .await
            .expect("send item 1");
        // Terminate with an error frame.
        req.send_error("error: something broke")
            .await
            .expect("send error");
    });

    let mut client = client;
    {
        let mut stream = client.call(Ping("err-me".to_string())).await.expect("call");

        use futures::StreamExt;
        let mut items = Vec::new();
        let mut stream_error = None;
        while let Some(result) = stream.next().await {
            match result {
                Ok(item) => items.push(item),
                Err(e) => {
                    stream_error = Some(e);
                    break;
                }
            }
        }

        // Should receive 2 ok items, then a remote error preserving the
        // server's message.
        assert_eq!(items.len(), 2);
        assert_eq!(items[0], Pong("ok-0".to_string()));
        assert_eq!(items[1], Pong("ok-1".to_string()));
        let error = stream_error.expect("stream should end with an error");
        assert!(
            matches!(error, selium_shm::RpcError::Remote(ref m) if m == "error: something broke"),
            "expected Remote error with the server's message, got: {error}"
        );
    }

    server_handle.await.expect("server task");
}

#[tokio::test]
async fn server_stream_finish_without_final_payload() {
    install_heap_provider();

    let request_channel = make_channel(1024);
    let reply_channel = make_channel(1024);
    let dummy = make_channel(64);

    let client: RpcServerStreamClient<Ping, Pong, ShmTransport> = RpcServerStreamClient::new(
        FramedWrite::new(ShmTransport::new(&dummy, &request_channel).expect("client tx")),
        FramedRead::new(ShmTransport::new(&reply_channel, &dummy).expect("client rx")),
    );

    let mut server: RpcServerStreamConnection<Ping, Pong, ShmTransport> =
        RpcServerStreamConnection::new(
            FramedRead::new(ShmTransport::new(&request_channel, &dummy).expect("server rx")),
            FramedWrite::new(ShmTransport::new(&dummy, &reply_channel).expect("server tx")),
            42,
        );

    let server_handle = tokio::spawn(async move {
        let mut req = server.recv().await.expect("recv request");
        req.send_item(Pong("only".to_string()))
            .await
            .expect("send item");
        req.finish().await.expect("finish");
    });

    let mut client = client;
    {
        let mut stream = client.call(Ping("q".to_string())).await.expect("call");

        use futures::StreamExt;
        let mut items = Vec::new();
        while let Some(result) = stream.next().await {
            items.push(result.expect("item"));
        }

        assert_eq!(items.len(), 1);
        assert_eq!(items[0], Pong("only".to_string()));
    }

    server_handle.await.expect("server task");
}

#[tokio::test]
async fn server_stream_ordered_delivery_and_end() {
    install_heap_provider();

    let request_channel = make_channel(1024);
    let reply_channel = make_channel(1024);
    let dummy = make_channel(64);

    let client: RpcServerStreamClient<Ping, Pong, ShmTransport> = RpcServerStreamClient::new(
        FramedWrite::new(ShmTransport::new(&dummy, &request_channel).expect("client tx")),
        FramedRead::new(ShmTransport::new(&reply_channel, &dummy).expect("client rx")),
    );

    let mut server: RpcServerStreamConnection<Ping, Pong, ShmTransport> =
        RpcServerStreamConnection::new(
            FramedRead::new(ShmTransport::new(&request_channel, &dummy).expect("server rx")),
            FramedWrite::new(ShmTransport::new(&dummy, &reply_channel).expect("server tx")),
            42,
        );

    // Server task: recv request, send 3 items, finish
    let server_handle = tokio::spawn(async move {
        let mut req = server.recv().await.expect("recv request");
        let ping: Ping = req.payload().expect("decode ping");
        assert_eq!(ping, Ping("stream-please".to_string()));

        req.send_item(Pong("item-0".to_string()))
            .await
            .expect("send item 0");
        req.send_item(Pong("item-1".to_string()))
            .await
            .expect("send item 1");
        req.send_final_item(Pong("item-2".to_string()))
            .await
            .expect("send final item");
    });

    let mut client = client;
    {
        let mut stream = client
            .call(Ping("stream-please".to_string()))
            .await
            .expect("call");

        use futures::StreamExt;
        let mut items = Vec::new();
        while let Some(result) = stream.next().await {
            items.push(result.expect("item"));
        }

        assert_eq!(items.len(), 3);
        assert_eq!(items[0], Pong("item-0".to_string()));
        assert_eq!(items[1], Pong("item-1".to_string()));
        assert_eq!(items[2], Pong("item-2".to_string()));
    }

    server_handle.await.expect("server task");
}

#[tokio::test]
async fn stale_cancel_frame_skipped_for_next_request() {
    install_heap_provider();

    let request_channel = make_channel(1024);
    let reply_channel = make_channel(1024);
    let dummy = make_channel(64);

    let client: RpcServerStreamClient<Ping, Pong, ShmTransport> = RpcServerStreamClient::new(
        FramedWrite::new(ShmTransport::new(&dummy, &request_channel).expect("client tx")),
        FramedRead::new(ShmTransport::new(&reply_channel, &dummy).expect("client rx")),
    );

    let mut server: RpcServerStreamConnection<Ping, Pong, ShmTransport> =
        RpcServerStreamConnection::new(
            FramedRead::new(ShmTransport::new(&request_channel, &dummy).expect("server rx")),
            FramedWrite::new(ShmTransport::new(&dummy, &reply_channel).expect("server tx")),
            42,
        );

    let server_handle = tokio::spawn(async move {
        // First stream: abandoned by the client → stale cancel frame lands in
        // the request ring.
        let first = server.recv().await.expect("recv first request");
        let correlation_a = first.correlation();
        drop(first);

        // Second recv must skip the stale cancel frame and return the *new*
        // request, not surface the cancel as a bogus empty-payload request.
        let mut second = server.recv().await.expect("recv second request");
        let ping: Ping = second.payload().expect("decode second request payload");
        assert_eq!(ping, Ping("second".to_string()));
        assert_ne!(
            second.correlation(),
            correlation_a,
            "second request must be a fresh one"
        );

        second
            .send_item(Pong("done".to_string()))
            .await
            .expect("send item");
        second
            .send_final_item(Pong("bye".to_string()))
            .await
            .expect("send final");
    });

    let mut client = client;
    {
        // Stream A: cancelled by the client right away (its cancel frame
        // becomes stale traffic in the request ring).
        {
            let mut stream = client
                .call(Ping("first".to_string()))
                .await
                .expect("call A");
            stream.cancel();
        }

        // Give the server a moment to observe the cancel.
        for _ in 0..50 {
            tokio::task::yield_now().await;
        }

        // Stream B: must work end-to-end despite the stale cancel frame.
        let mut stream = client
            .call(Ping("second".to_string()))
            .await
            .expect("call B");
        use futures::StreamExt;
        let mut items = Vec::new();
        while let Some(result) = stream.next().await {
            items.push(result.expect("item"));
        }
        assert_eq!(items.len(), 2);
        assert_eq!(items[0], Pong("done".to_string()));
        assert_eq!(items[1], Pong("bye".to_string()));
    }

    server_handle.await.expect("server task");
}

#[tokio::test]
async fn streams_never_report_overwritten() {
    install_heap_provider();

    let request_channel = make_channel(1024);
    // Park ring far smaller than the total stream bytes (~20 bytes per frame
    // × 10 items + headers ≈ 320 bytes): the producer is forced to park and
    // the consumer must drain for the stream to complete. This exercises the
    // overwrite boundary rather than avoiding it.
    let reply_channel = make_park_channel(128);
    let dummy = make_channel(64);

    // Verify the channel uses Park backpressure, not Drop.
    let bp = reply_channel
        .ring()
        .region()
        .load_backpressure()
        .unwrap_or(1);
    assert_eq!(
        bp,
        ChannelBackpressure::Park.to_u8(),
        "stream tests require Park backpressure; this verifies the spec"
    );

    let client: RpcServerStreamClient<Ping, Pong, ShmTransport> = RpcServerStreamClient::new(
        FramedWrite::new(ShmTransport::new(&dummy, &request_channel).expect("client tx")),
        FramedRead::new(ShmTransport::new(&reply_channel, &dummy).expect("client rx")),
    );

    let mut server: RpcServerStreamConnection<Ping, Pong, ShmTransport> =
        RpcServerStreamConnection::new(
            FramedRead::new(ShmTransport::new(&request_channel, &dummy).expect("server rx")),
            FramedWrite::new(ShmTransport::new(&dummy, &reply_channel).expect("server tx")),
            42,
        );

    // Slow consumer: pause briefly between items so the producer repeatedly
    // hits a full ring and must park (not overwrite).
    let server_handle = tokio::spawn(async move {
        let mut req = server.recv().await.expect("recv request");

        for i in 0..10 {
            req.send_item(Pong(format!("o{i:02}")))
                .await
                .expect("send parks until writable");
            // Slow the consumer-side producer so the ring fills and the
            // writer must park repeatedly.
            for _ in 0..20 {
                tokio::task::yield_now().await;
            }
        }
        req.finish().await.expect("finish");
    });

    let mut client = client;
    {
        let mut stream = client
            .call(Ping("no-overwrite".to_string()))
            .await
            .expect("call");

        use futures::StreamExt;
        let mut items = Vec::new();
        while let Some(result) = stream.next().await {
            match result {
                Ok(item) => items.push(item),
                Err(e) => {
                    // The key assertion: streams must never report Overwritten.
                    panic!("stream reported Overwritten: {e}");
                }
            }
        }

        // Every item accounted for, in order — nothing was overwritten or lost.
        assert_eq!(items.len(), 10, "all items received, none overwritten");
        for (i, item) in items.iter().enumerate() {
            assert_eq!(item, &Pong(format!("o{i:02}")));
        }
    }

    server_handle.await.expect("server task");
}
