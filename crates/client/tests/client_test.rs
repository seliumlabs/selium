//! Native integration tests for `selium-client`.
//!
//! Each test drives the client against a real QUIC (TLS 1.3) server built with
//! the connector's production server-config seam (`build_server_config`),
//! over a loopback tokio socket. The server halves speak `selium-wire`
//! framing and the deterministic bridge handshake (`Accepted` on success,
//! `Terminate` on refusal), so the tests exercise the full
//! connect → handshake → channel round-trip path that hand-rolled users
//! would otherwise write themselves.

use std::{net::SocketAddr, sync::Arc, time::Duration};

use futures::{SinkExt, StreamExt};
use quinn::rustls::pki_types::{CertificateDer, PrivateKeyDer};
use selium_client::{ClientIdentity, ConnectOptions, FlatMsg, QuicTransport};
use selium_wire::{FramedRead, FramedWrite, PipeControl};

const CLIENT_CERT_PEM: &[u8] =
    include_bytes!("../../../guests/connector-quic/tests/fixtures/client_cert.pem");
const CLIENT_KEY_PEM: &[u8] =
    include_bytes!("../../../guests/connector-quic/tests/fixtures/client_key.pem");
const SERVER_CERT_PEM: &[u8] =
    include_bytes!("../../../guests/connector-quic/tests/fixtures/cert.pem");
const SERVER_KEY_PEM: &[u8] =
    include_bytes!("../../../guests/connector-quic/tests/fixtures/key.pem");
const SERVER_NAME: &str = "localhost";

/// 6.1 (extended): a bad-handshake termination code maps to the bad-handshake
/// variant.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bad_handshake_termination_maps_to_bad_handshake_error() {
    let (addr, server) = bind_server(plain_server_config()).await;
    let bridge = tokio::spawn(async move {
        let connection = server
            .accept()
            .await
            .expect("accept")
            .await
            .expect("handshake");
        let (send, recv) = connection.accept_bi().await.expect("stream");

        let mut reader = FramedRead::new(QuicTransport::read_only(recv));
        let mut writer = FramedWrite::new(QuicTransport::write_only(send));

        handshake_and_terminate(
            &mut reader,
            &mut writer,
            "sel://acme/garbage",
            selium_wire::TERMINATE_BAD_HANDSHAKE,
        )
        .await;
        keep_connection_alive().await;
    });

    let client = selium_client::connect(addr, plain_options())
        .await
        .expect("connect");
    let error = tokio::time::timeout(
        Duration::from_secs(5),
        expect_open_error(client.subscriber::<String>("sel://acme/garbage")),
    )
    .await
    .expect("termination within timeout");
    assert!(matches!(error, selium_client::Error::BadHandshake));

    bridge.abort();
}

/// 5.4: a bidirectional-streaming RPC round-trips items in both directions
/// on one session.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bidi_stream_round_trip() {
    let (addr, server) = bind_server(plain_server_config()).await;
    let bridge = tokio::spawn(async move {
        let connection = server
            .accept()
            .await
            .expect("accept")
            .await
            .expect("handshake");
        let (send, recv) = connection.accept_bi().await.expect("stream");

        let mut reader = FramedRead::new(QuicTransport::read_only(recv));
        let mut writer = FramedWrite::new(QuicTransport::write_only(send));

        handshake_and_accept(&mut reader, &mut writer, "sel://acme/bidi").await;

        // The fabric side of the session: echo every request item back.
        let mut connection =
            selium_wire::RpcBidiStreamConnection::<String, String, String, QuicTransport>::new(
                reader, writer, 0,
            );
        let mut request = connection.recv().await.expect("recv request");
        assert_eq!(
            request.payload().expect("decode request"),
            "start".to_string()
        );
        let (mut responder, mut requests) = request.split();
        while let Some(item) = requests.recv().await.expect("request items") {
            responder.send(item).await.expect("echo item");
        }
        responder.close().await.expect("close responder");
        // Hold the connection open: dropping it would discard undelivered
        // stream data before the client drains the session.
        keep_connection_alive().await;
    });

    let client = selium_client::connect(addr, plain_options())
        .await
        .expect("connect");
    let mut bidi_client = client
        .bidi_stream::<String, String, String>("sel://acme/bidi")
        .await
        .expect("bidi client");
    let mut stream = bidi_client
        .connect("start".to_string())
        .await
        .expect("connect");
    let (mut sender, mut receiver) = stream.split();

    sender.send("alpha".to_string()).await.expect("send 1");
    sender.send("beta".to_string()).await.expect("send 2");
    sender.close().await.expect("close sender");

    let item = tokio::time::timeout(Duration::from_secs(5), receiver.recv())
        .await
        .expect("reply 1 within timeout")
        .expect("reply present")
        .expect("decode");
    assert_eq!(item, "alpha");
    let item = tokio::time::timeout(Duration::from_secs(5), receiver.recv())
        .await
        .expect("reply 2 within timeout")
        .expect("reply present")
        .expect("decode");
    assert_eq!(item, "beta");
    let end = tokio::time::timeout(Duration::from_secs(5), receiver.recv())
        .await
        .expect("end within timeout")
        .expect("recv end");
    assert!(end.is_none(), "session ends after the responder closes");

    bridge.abort();
}

/// Binds a native QUIC server on loopback and returns its address + endpoint.
async fn bind_server(config: quinn::ServerConfig) -> (SocketAddr, quinn::Endpoint) {
    let endpoint =
        quinn::Endpoint::server(config, "127.0.0.1:0".parse().expect("bind addr")).expect("server");
    let addr = endpoint.local_addr().expect("local addr");
    (addr, endpoint)
}

/// 5.1: channel open writes the typed handshake, the bridge replies accepted,
/// and a following data frame carries the publisher's correlation tag.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn channel_open_writes_handshake_then_tagged_frame() {
    let (addr, server) = bind_server(plain_server_config()).await;
    let bridge = tokio::spawn(async move {
        let connection = server
            .accept()
            .await
            .expect("accept")
            .await
            .expect("handshake");
        let (send, recv) = connection.accept_bi().await.expect("stream");

        let mut reader = FramedRead::new(QuicTransport::read_only(recv));
        let mut writer = FramedWrite::new(QuicTransport::write_only(send));

        handshake_and_accept(&mut reader, &mut writer, "sel://acme/lobby").await;

        let (payload, tag, _flags) = reader.read_frame_async().await.expect("data frame");
        assert_eq!(tag, 6, "data frame carries the publisher writer id");
        let message = String::decode(&payload).expect("decode");
        assert_eq!(message, "hello");
    });

    let client = selium_client::connect(addr, plain_options())
        .await
        .expect("connect");
    let mut publisher = client
        .publisher::<String>("sel://acme/lobby")
        .await
        .expect("publisher");
    publisher.set_writer_id(6);
    publisher.publish(&"hello".to_string()).expect("publish");

    bridge.await.expect("bridge task");
}

fn client_certs() -> Vec<CertificateDer<'static>> {
    selium_client::certificates_from_pem(CLIENT_CERT_PEM).expect("client cert PEM")
}

fn client_key() -> PrivateKeyDer<'static> {
    selium_client::private_key_from_pem(CLIENT_KEY_PEM).expect("client key PEM")
}

/// Spec: Mutual TLS Client Identity — an identity in the connect options
/// completes mutual TLS through the connector's mTLS server config.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn client_presents_identity_for_mutual_tls() {
    let (addr, server) = bind_server(mtls_server_config()).await;
    let accept = tokio::spawn(async move {
        let _connection = server
            .accept()
            .await
            .expect("accept")
            .await
            .expect("handshake");
    });

    let client = selium_client::connect(addr, identity_options())
        .await
        .expect("connect with client identity");
    drop(client);
    accept.await.expect("server task");
}

/// 7.1: `connect` + a channel round-trip through the connector seam, driven
/// end to end on one connection spanning two channel streams.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn connect_and_channel_round_trip() {
    let (addr, server) = bind_server(plain_server_config()).await;
    let bridge = tokio::spawn(async move {
        let connection = server
            .accept()
            .await
            .expect("accept")
            .await
            .expect("handshake");

        // Publisher channel stream: complete its handshake before the
        // subscriber stream is opened (the client awaits the accepted reply
        // on each stream before opening the next).
        let (a_send, a_recv) = connection.accept_bi().await.expect("publisher stream");
        let mut a_reader = FramedRead::new(QuicTransport::read_only(a_recv));
        let mut a_writer = FramedWrite::new(QuicTransport::write_only(a_send));
        handshake_and_accept(&mut a_reader, &mut a_writer, "sel://acme/publisher").await;

        // Subscriber channel stream.
        let (b_send, b_recv) = connection.accept_bi().await.expect("subscriber stream");
        let mut b_reader = FramedRead::new(QuicTransport::read_only(b_recv));
        let mut b_writer = FramedWrite::new(QuicTransport::write_only(b_send));
        handshake_and_accept(&mut b_reader, &mut b_writer, "sel://acme/subscriber").await;

        // Relay the publisher's message onto the subscriber's channel.
        let (payload, tag, _flags) = a_reader.read_frame_async().await.expect("publish");
        b_writer.write_frame(&payload, tag).expect("relay");
        keep_connection_alive().await;
    });

    let client = selium_client::connect(addr, plain_options())
        .await
        .expect("connect");
    let mut publisher = client
        .publisher::<String>("sel://acme/publisher")
        .await
        .expect("publisher");
    let mut subscriber = client
        .subscriber::<String>("sel://acme/subscriber")
        .await
        .expect("subscriber");

    publisher
        .publish(&"round trip".to_string())
        .expect("publish");

    let item = tokio::time::timeout(Duration::from_secs(5), subscriber.next())
        .await
        .expect("relayed within timeout")
        .expect("stream open")
        .expect("decode");
    assert_eq!(item, "round trip");

    bridge.abort();
}

/// 4.3: `connect` returns a `Client` owning the connection, with a trusted root.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn connect_returns_client_with_trusted_root() {
    let (addr, server) = bind_server(plain_server_config()).await;
    let accept = tokio::spawn(async move {
        let connection = server
            .accept()
            .await
            .expect("accept")
            .await
            .expect("handshake");
        drop(connection);
    });

    let client = selium_client::connect(addr, plain_options())
        .await
        .expect("connect");
    drop(client);
    accept.await.expect("server task");
}

/// 6.1: a bridge termination frame surfaces the typed attach-failed error at
/// channel open (the handshake reply is deterministic: refusal is a typed
/// `Terminate`, not silence).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn denied_attach_returns_typed_error_at_open() {
    let (addr, server) = bind_server(plain_server_config()).await;
    let bridge = tokio::spawn(async move {
        let connection = server
            .accept()
            .await
            .expect("accept")
            .await
            .expect("handshake");
        let (send, recv) = connection.accept_bi().await.expect("stream");

        let mut reader = FramedRead::new(QuicTransport::read_only(recv));
        let mut writer = FramedWrite::new(QuicTransport::write_only(send));

        handshake_and_terminate(
            &mut reader,
            &mut writer,
            "sel://acme/forbidden",
            selium_wire::TERMINATE_ATTACH_FAILED,
        )
        .await;
        keep_connection_alive().await;
    });

    let client = selium_client::connect(addr, plain_options())
        .await
        .expect("connect");
    let error = tokio::time::timeout(
        Duration::from_secs(5),
        expect_open_error(client.subscriber::<String>("sel://acme/forbidden")),
    )
    .await
    .expect("denial within timeout");
    assert!(matches!(error, selium_client::Error::AttachFailed));

    bridge.abort();
}

/// 6.1 (extended): the typed denial surfaces at open for every handle shape —
/// here an RPC channel and a publisher channel on the same connection.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn denied_attach_surfaces_typed_errors_for_rpc_and_publisher() {
    let (addr, server) = bind_server(plain_server_config()).await;
    let bridge = tokio::spawn(async move {
        let connection = server
            .accept()
            .await
            .expect("accept")
            .await
            .expect("handshake");

        // RPC channel refusal.
        let (send, recv) = connection.accept_bi().await.expect("rpc stream");
        let mut reader = FramedRead::new(QuicTransport::read_only(recv));
        let mut writer = FramedWrite::new(QuicTransport::write_only(send));
        handshake_and_terminate(
            &mut reader,
            &mut writer,
            "sel://acme/rpc-denied",
            selium_wire::TERMINATE_ATTACH_FAILED,
        )
        .await;

        // Publisher channel refusal.
        let (send, recv) = connection.accept_bi().await.expect("publisher stream");
        let mut reader = FramedRead::new(QuicTransport::read_only(recv));
        let mut writer = FramedWrite::new(QuicTransport::write_only(send));
        handshake_and_terminate(
            &mut reader,
            &mut writer,
            "sel://acme/pub-denied",
            selium_wire::TERMINATE_ATTACH_FAILED,
        )
        .await;

        keep_connection_alive().await;
    });

    let client = selium_client::connect(addr, plain_options())
        .await
        .expect("connect");

    let error = tokio::time::timeout(
        Duration::from_secs(5),
        expect_open_error(client.rpc::<String, String>("sel://acme/rpc-denied")),
    )
    .await
    .expect("rpc denial within timeout");
    assert!(matches!(error, selium_client::Error::AttachFailed));

    let error = tokio::time::timeout(
        Duration::from_secs(5),
        expect_open_error(client.publisher::<String>("sel://acme/pub-denied")),
    )
    .await
    .expect("publisher denial within timeout");
    assert!(matches!(error, selium_client::Error::AttachFailed));

    bridge.abort();
}

/// Awaits a channel-open future that must fail, returning its typed error
/// (the handle types are not `Debug`, so `expect_err` is unavailable).
async fn expect_open_error<T>(
    open: impl std::future::Future<Output = selium_client::Result<T>>,
) -> selium_client::Error {
    open.await
        .err()
        .expect("channel open must fail with a typed error")
}

/// 6.2: `FlatMsg` and the encoding crate are reachable from the client alone.
#[test]
fn flat_msg_and_encoding_are_re_exported() {
    fn assert_bound<T: selium_client::FlatMsg>() {}
    assert_bound::<String>();
    assert_bound::<u64>();

    // A user type can round-trip through the re-exported trait.
    let bytes = <String as selium_client::FlatMsg>::encode(&"hello".to_string());
    let decoded: String = <String as selium_client::FlatMsg>::decode(&bytes).expect("decode");
    assert_eq!(decoded, "hello");

    // The encoding crate itself is reachable for schema types.
    let _: Option<selium_client::selium_service::EncodingError> = None;
}

/// Reads the handshake frame from a bridge peer, asserts its URI, and replies
/// with the deterministic accepted control frame.
async fn handshake_and_accept(
    reader: &mut FramedRead<QuicTransport>,
    writer: &mut FramedWrite<QuicTransport>,
    uri: &str,
) {
    let (payload, tag, _flags) = reader.read_frame_async().await.expect("handshake frame");
    assert_eq!(tag, 0, "handshake carries control tag 0");
    assert_eq!(
        <PipeControl as FlatMsg>::decode(&payload).expect("control frame"),
        PipeControl::Handshake {
            uri: uri.to_string()
        }
    );
    let accepted = FlatMsg::encode(&PipeControl::Accepted);
    writer.write_frame(&accepted, 0).expect("accepted reply");
}

/// Reads the handshake frame from a bridge peer and replies with a typed
/// termination frame (the refusal path of the deterministic handshake).
async fn handshake_and_terminate(
    reader: &mut FramedRead<QuicTransport>,
    writer: &mut FramedWrite<QuicTransport>,
    uri: &str,
    code: u32,
) {
    let (payload, tag, _flags) = reader.read_frame_async().await.expect("handshake frame");
    assert_eq!(tag, 0, "handshake carries control tag 0");
    assert_eq!(
        <PipeControl as FlatMsg>::decode(&payload).expect("control frame"),
        PipeControl::Handshake {
            uri: uri.to_string()
        }
    );
    let terminate = FlatMsg::encode(&PipeControl::Terminate { code });
    writer.write_frame(&terminate, 0).expect("terminate reply");
}

fn identity_options() -> ConnectOptions {
    ConnectOptions {
        server_name: SERVER_NAME.to_string(),
        server_root: server_certs(),
        identity: Some(ClientIdentity {
            cert_chain: client_certs(),
            key: client_key(),
        }),
        transport: None,
    }
}

/// Keeps a bridge connection alive until the test aborts its task.
async fn keep_connection_alive() {
    std::future::pending::<()>().await
}

/// 5.5: a live table round-trips set/get/delete over the async sync path.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_table_set_get_delete_round_trip() {
    let (addr, server) = bind_server(plain_server_config()).await;
    let bridge = tokio::spawn(async move {
        let connection = server
            .accept()
            .await
            .expect("accept")
            .await
            .expect("handshake");
        let (send, recv) = connection.accept_bi().await.expect("stream");

        let mut reader = FramedRead::new(QuicTransport::read_only(recv));
        let mut writer = FramedWrite::new(QuicTransport::write_only(send));

        handshake_and_accept(&mut reader, &mut writer, "sel://acme/table").await;

        // The fabric replays every mutation back to the topic; loop echoing.
        while let Ok((payload, tag, flags)) = reader.read_frame_async().await {
            if writer.write_frame_with_flags(&payload, tag, flags).is_err() {
                break;
            }
        }
    });

    let client = selium_client::connect(addr, plain_options())
        .await
        .expect("connect");
    let table = client
        .live_table::<String, u64>("sel://acme/table")
        .await
        .expect("live table");

    table
        .set_async("alpha".to_string(), 10u64)
        .await
        .expect("set");
    assert_eq!(table.get(&"alpha".to_string()).expect("get"), Some(10u64));

    table
        .delete_async("alpha".to_string())
        .await
        .expect("delete");
    assert_eq!(table.get(&"alpha".to_string()).expect("get"), None);

    bridge.abort();
}

/// The server's mTLS config, mirroring the connector's `build_server_config`
/// (TLS 1.3 + mandatory client auth against the test client anchor).
fn mtls_server_config() -> quinn::ServerConfig {
    use quinn::rustls::{
        RootCertStore, crypto::ring::default_provider, server::WebPkiClientVerifier, version::TLS13,
    };

    let mut roots = RootCertStore::empty();
    roots
        .add(client_certs().into_iter().next().expect("client leaf cert"))
        .expect("trust client cert");

    let builder = quinn::rustls::ServerConfig::builder_with_provider(Arc::new(default_provider()))
        .with_protocol_versions(&[&TLS13])
        .expect("ring provider supports TLS 1.3");
    let client_verifier = WebPkiClientVerifier::builder(Arc::new(roots))
        .build()
        .expect("client verifier");
    let rustls = builder
        .with_client_cert_verifier(client_verifier)
        .with_single_cert(server_certs(), server_key())
        .expect("mTLS server config");
    quinn::ServerConfig::with_crypto(Arc::new(
        quinn::crypto::rustls::QuicServerConfig::try_from(rustls)
            .expect("quic server crypto config"),
    ))
}

fn plain_options() -> ConnectOptions {
    ConnectOptions {
        server_name: SERVER_NAME.to_string(),
        server_root: server_certs(),
        identity: None,
        transport: None,
    }
}

/// The server's no-mTLS config, mirroring the connector's `build_server_config`
/// seam (TLS 1.3, ring provider, no client auth).
fn plain_server_config() -> quinn::ServerConfig {
    quinn::ServerConfig::with_single_cert(server_certs(), server_key()).expect("server config")
}

/// 5.3: a publisher is a `futures::Sink<T>` whose `send` emits an encoded frame.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn publisher_sends_encoded_message() {
    let (addr, server) = bind_server(plain_server_config()).await;
    let bridge = tokio::spawn(async move {
        let connection = server
            .accept()
            .await
            .expect("accept")
            .await
            .expect("handshake");
        let (send, recv) = connection.accept_bi().await.expect("stream");

        let mut reader = FramedRead::new(QuicTransport::read_only(recv));
        let mut writer = FramedWrite::new(QuicTransport::write_only(send));

        handshake_and_accept(&mut reader, &mut writer, "sel://acme/lobby").await;
        let (payload, _tag, _flags) = reader.read_frame_async().await.expect("data frame");
        let message = String::decode(&payload).expect("decode");
        assert_eq!(message, "encoded message");
    });

    let client = selium_client::connect(addr, plain_options())
        .await
        .expect("connect");
    let mut publisher = client
        .publisher::<String>("sel://acme/lobby")
        .await
        .expect("publisher");

    publisher
        .send("encoded message".to_string())
        .await
        .expect("sink send");

    bridge.await.expect("bridge task");
}

/// 5.4: request/response RPC round-trips a request and its correlation tag.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rpc_request_reply_round_trip() {
    let (addr, server) = bind_server(plain_server_config()).await;
    let bridge = tokio::spawn(async move {
        let connection = server
            .accept()
            .await
            .expect("accept")
            .await
            .expect("handshake");
        let (send, recv) = connection.accept_bi().await.expect("stream");

        let mut reader = FramedRead::new(QuicTransport::read_only(recv));
        let mut writer = FramedWrite::new(QuicTransport::write_only(send));

        handshake_and_accept(&mut reader, &mut writer, "sel://acme/api").await;
        let (payload, tag, _flags) = reader.read_frame_async().await.expect("request frame");
        assert_eq!(String::decode(&payload).expect("decode request"), "ping");
        writer
            .write_frame(FlatMsg::encode(&"pong".to_string()).as_slice(), tag)
            .expect("reply");
        keep_connection_alive().await;
    });

    let client = selium_client::connect(addr, plain_options())
        .await
        .expect("connect");
    let mut rpc = client
        .rpc::<String, String>("sel://acme/api")
        .await
        .expect("rpc");

    let reply = tokio::time::timeout(Duration::from_secs(5), rpc.request("ping".to_string()))
        .await
        .expect("reply within timeout")
        .expect("request");
    assert_eq!(reply, "pong");

    bridge.abort();
}

fn server_certs() -> Vec<CertificateDer<'static>> {
    selium_client::certificates_from_pem(SERVER_CERT_PEM).expect("server cert PEM")
}

fn server_key() -> PrivateKeyDer<'static> {
    selium_client::private_key_from_pem(SERVER_KEY_PEM).expect("server key PEM")
}

/// 5.4: a server-streaming RPC round-trips a request into a stream of items.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn server_stream_round_trip() {
    let (addr, server) = bind_server(plain_server_config()).await;
    let bridge = tokio::spawn(async move {
        let connection = server
            .accept()
            .await
            .expect("accept")
            .await
            .expect("handshake");
        let (send, recv) = connection.accept_bi().await.expect("stream");

        let mut reader = FramedRead::new(QuicTransport::read_only(recv));
        let mut writer = FramedWrite::new(QuicTransport::write_only(send));

        handshake_and_accept(&mut reader, &mut writer, "sel://acme/stream").await;

        // The fabric side of the stream: a typed server-streaming responder.
        let mut connection =
            selium_wire::RpcServerStreamConnection::<String, String, QuicTransport>::new(
                reader, writer, 0,
            );
        let mut request = connection.recv().await.expect("recv request");
        assert_eq!(
            request.payload().expect("decode request"),
            "count".to_string()
        );
        request.send_item("one".to_string()).await.expect("item 1");
        request
            .send_final_item("two".to_string())
            .await
            .expect("item 2");
        // Hold the connection open: dropping it would discard undelivered
        // stream data before the client drains the items.
        keep_connection_alive().await;
    });

    let client = selium_client::connect(addr, plain_options())
        .await
        .expect("connect");
    let mut stream_client = client
        .server_stream::<String, String>("sel://acme/stream")
        .await
        .expect("server-stream client");
    let mut stream = stream_client.call("count".to_string()).await.expect("call");

    let item = tokio::time::timeout(Duration::from_secs(5), stream.next())
        .await
        .expect("item 1 within timeout")
        .expect("stream open")
        .expect("decode");
    assert_eq!(item, "one");
    let item = tokio::time::timeout(Duration::from_secs(5), stream.next())
        .await
        .expect("item 2 within timeout")
        .expect("final item present")
        .expect("decode");
    assert_eq!(item, "two");
    let end = tokio::time::timeout(Duration::from_secs(5), stream.next())
        .await
        .expect("end within timeout");
    assert!(end.is_none(), "final item ends the stream");

    bridge.abort();
}

/// 5.2: a subscriber yields decoded values end-to-end.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn subscriber_reads_decoded_values_end_to_end() {
    let (addr, server) = bind_server(plain_server_config()).await;
    let bridge = tokio::spawn(async move {
        let connection = server
            .accept()
            .await
            .expect("accept")
            .await
            .expect("handshake");
        let (send, recv) = connection.accept_bi().await.expect("stream");

        let mut reader = FramedRead::new(QuicTransport::read_only(recv));
        let mut writer = FramedWrite::new(QuicTransport::write_only(send));

        handshake_and_accept(&mut reader, &mut writer, "sel://acme/lobby").await;
        writer
            .write_frame(FlatMsg::encode(&"greetings".to_string()).as_slice(), 7)
            .expect("write value");
        keep_connection_alive().await;
    });

    let client = selium_client::connect(addr, plain_options())
        .await
        .expect("connect");
    let mut subscriber = client
        .subscriber::<String>("sel://acme/lobby")
        .await
        .expect("subscriber");

    let item = tokio::time::timeout(Duration::from_secs(5), subscriber.next())
        .await
        .expect("item within timeout")
        .expect("stream open")
        .expect("decode");
    assert_eq!(item, "greetings");

    bridge.abort();
}

/// 4.2: the TLS builder produces a client config that completes a handshake
/// against the connector's server-config seam.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tls_builder_handshake_completes_against_connector_seam() {
    let (addr, server) = bind_server(plain_server_config()).await;
    let accept = tokio::spawn(async move {
        let connection = server
            .accept()
            .await
            .expect("accept")
            .await
            .expect("handshake");
        drop(connection);
    });

    let config = selium_client::build_client_config(&plain_options()).expect("client config");
    let mut endpoint =
        quinn::Endpoint::client("127.0.0.1:0".parse().expect("bind addr")).expect("client");
    endpoint.set_default_client_config(config);

    let connection = tokio::time::timeout(
        Duration::from_secs(5),
        endpoint.connect(addr, SERVER_NAME).expect("connect"),
    )
    .await
    .expect("handshake within timeout")
    .expect("connection");
    drop(connection);
    drop(endpoint);
    accept.await.expect("server task");
}
