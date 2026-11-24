//! External HTTPS client → HTTP connector → real application guest.
//!
//! This is the external-path evidence for the `http-connector` capability:
//! a plain `rustls`/`tokio` client (no Selium SDK) completes a TLS handshake
//! against the real `selium-connector-http` WASM guest, is routed by the
//! request Host through the real discovery guest to the real
//! `selium-http-demo` application guest, and receives the guest's response
//! byte-for-byte. It deliberately exercises only public boundaries: the
//! connector's socket, the HTTP wire format, and the guest's served route.
//!
//! `#[ignore]`d by default because the guests must be built for
//! `wasm32-unknown-unknown` first (release profile — the interpreted TLS
//! handshake is too slow at debug optimisation):
//!
//! ```sh
//! cargo build --release --target wasm32-unknown-unknown \
//!   -p selium-discovery -p selium-store -p selium-connector-http -p selium-http-demo
//! cargo test -p selium-runtime --test https_spine -- --ignored
//! ```

use std::{sync::Arc, time::Duration};

use selium_abi::{Capability, CapabilityGrant, ResourceClass, ResourceSelector};
use selium_runtime::{RuntimeConfig, SystemGuestArg, SystemGuestDescriptor};
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::TcpStream,
};
use tokio_rustls::TlsConnector;

mod common;
mod spine_common;

use spine_common::{
    BRIDGE_CERT_PEM, SpinePortGuard, StateRoot, discovery_descriptor, read_wasm,
    runtime_in_state_root, seed_tls_blob_store, store_descriptor, wait_for_logs,
};

/// The connector's listener for this test. Port 443 needs privileges a test
/// lane does not have; the connector takes the address as a bootstrap
/// argument precisely so an external path can be exercised unprivileged.
const CONNECTOR_ADDR: &str = "127.0.0.1:8443";
/// The single-label Host that projects onto the root namespace, where the
/// demo guest serves `sel:///`.
const REQUEST_HOST: &str = "demo";
/// Bound on the interpreted TLS handshake and the guest round trip.
const EXCHANGE_BOUND: Duration = Duration::from_secs(300);
/// Bound on observing the guests' own startup logs.
const OBSERVATION_BOUND: Duration = Duration::from_secs(240);

// scenario: verification-gate/external-protocol-paths-have-real-end-to-end-evidence/external-https-client-reaches-application-guest
// scenario: http-connector/edge-termination-of-http-1-1-over-tls/browser-grade-request
// scenario: http-connector/discovery-based-route-resolution/request-routed-to-registered-guest
// scenario: http-connector/discovery-based-route-resolution/no-route-registered
// scenario: http-connector/streaming-bodies-via-server-streaming-rpc/chunked-response-streamed-to-the-wire
// scenario: http-connector/streaming-bodies-via-server-streaming-rpc/oversized-request-rejected-at-the-edge
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires the discovery, store, HTTP connector and HTTP demo guests built for wasm32-unknown-unknown"]
async fn external_https_client_reaches_a_real_application_guest() {
    let _port = SpinePortGuard::acquire();

    let state = StateRoot::new("https-spine");
    let runtime = runtime_in_state_root(&state);
    seed_tls_blob_store(&runtime);

    let report = runtime
        .bootstrap_system_guests(RuntimeConfig {
            start_discovery: true,
            system_guests: vec![
                discovery_descriptor(read_wasm("selium-discovery", "selium_discovery.wasm")),
                store_descriptor(vec!["discovery".to_string()]),
                http_connector_descriptor(CONNECTOR_ADDR),
                http_demo_descriptor(),
            ],
            state_dir: state.config_dir(),
        })
        .unwrap_or_else(|error| {
            let activity: Vec<String> = runtime
                .activity_log()
                .into_iter()
                .map(|event| format!("activity: {}", event.message))
                .collect();
            panic!("bootstrap HTTPS spine: {error}; state: {activity:#?}")
        });

    let find = |name: &str| {
        report
            .guests
            .iter()
            .find(|guest| guest.name == name)
            .unwrap_or_else(|| panic!("bootstrap report contains guest {name}"))
            .process_id
    };
    let connector_pid = find("http-connector");
    let demo = find("http-demo");

    let _ = wait_for_logs(
        &runtime,
        connector_pid,
        &[
            ("http-connector: TLS configured", 1),
            ("http-connector: bound to", 1),
        ],
        OBSERVATION_BOUND,
    );
    let _ = wait_for_logs(
        &runtime,
        demo,
        &[("http-demo: serving", 1), ("http-demo: serving stream", 1)],
        OBSERVATION_BOUND,
    );

    // A plain TLS client: no Selium SDK, no connector internals.
    let mut roots = rustls::RootCertStore::empty();
    for certificate in
        selium_client::certificates_from_pem(BRIDGE_CERT_PEM).expect("parse server certificate")
    {
        roots.add(certificate).expect("trust the connector root");
    }
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .expect("TLS versions")
        .with_root_certificates(roots)
        .with_no_client_auth();
    let connector = TlsConnector::from(Arc::new(config));

    let exchange = async {
        let stream = TcpStream::connect(CONNECTOR_ADDR)
            .await
            .expect("connect to the connector");
        let server_name = rustls_pki_types::ServerName::try_from("bridge").expect("server name");
        let mut stream = connector
            .connect(server_name, stream)
            .await
            .expect("TLS handshake");

        let body = b"hello from the external client";
        let request = format!(
            "POST / HTTP/1.1\r\nhost: {REQUEST_HOST}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
            body.len(),
        );
        stream
            .write_all(request.as_bytes())
            .await
            .expect("write request head");
        stream.write_all(body).await.expect("write request body");
        stream.flush().await.expect("flush request");

        read_http_response(&mut stream).await
    };

    let (status, body) = match tokio::time::timeout(EXCHANGE_BOUND, exchange).await {
        Ok(response) => response,
        Err(_elapsed) => {
            let mut logs = Vec::new();
            for process_id in [connector_pid, demo] {
                if let Ok(messages) = runtime.kernel().processes().drain_log_channel(process_id) {
                    logs.extend(messages.into_iter().filter_map(|frame| {
                        selium_service::FlatMsg::decode(&frame)
                            .ok()
                            .map(|record: selium_service::log::LogRecord| record.message)
                    }));
                }
            }
            panic!("external HTTPS exchange timed out; guest logs: {logs:#?}");
        }
    };
    assert_eq!(
        status, 200,
        "the application guest's status reaches the wire"
    );
    let expected = format!(
        "POST /\n{}",
        String::from_utf8_lossy(b"hello from the external client"),
    );
    assert_eq!(
        String::from_utf8_lossy(&body),
        expected,
        "the request crossed connector and guest byte-for-byte"
    );

    // A streaming route: the app guest sends a head then body chunks, and the
    // connector writes them to the wire with chunked transfer encoding. The
    // whole body arrives with no connector-side buffering requirement.
    let streamed = tokio::time::timeout(EXCHANGE_BOUND, async {
        let mut stream = connect_external(&connector).await;
        stream
            .write_all(b"GET /events HTTP/1.1\r\nhost: demo\r\nconnection: close\r\n\r\n")
            .await
            .expect("write streaming request");
        read_chunked_response(&mut stream).await
    })
    .await
    .expect("streaming exchange");
    assert_eq!(
        String::from_utf8_lossy(&streamed),
        "streamed-in-chunks",
        "chunked body arrives in order and complete"
    );

    // Exceed the dispatch pipeline bound (eight in-flight requests) on one
    // external connection. Half-close the write side, then verify that bounded
    // forwarding neither drops nor reorders a response while the socket and
    // guest channels apply backpressure.
    let pipelined = tokio::time::timeout(EXCHANGE_BOUND, async {
        const REQUESTS: usize = 12;
        let mut stream = connect_external(&connector).await;
        let request = b"GET /events HTTP/1.1\r\nhost: demo\r\n\r\n";
        for _ in 0..REQUESTS {
            stream
                .write_all(request)
                .await
                .expect("write pipelined request");
        }
        stream
            .shutdown()
            .await
            .expect("half-close pipelined requests");
        let mut wire = Vec::new();
        if let Err(error) = stream.read_to_end(&mut wire).await {
            // The connector closes its TCP transport after draining replies
            // without a TLS close_notify. All ordered response bytes have
            // already been read; rustls reports that transport EOF explicitly.
            assert_eq!(error.kind(), std::io::ErrorKind::UnexpectedEof);
        }
        (wire, REQUESTS)
    })
    .await
    .expect("backpressured pipeline exchange");
    let statuses = pipelined
        .0
        .windows(b"HTTP/1.1 200".len())
        .filter(|window| *window == b"HTTP/1.1 200")
        .count();
    let bodies = pipelined
        .0
        .windows(b"streamed-".len())
        .filter(|window| *window == b"streamed-")
        .count();
    assert_eq!(statuses, pipelined.1, "every reply survives backpressure");
    assert_eq!(bodies, pipelined.1, "every streamed body remains intact");

    // Actual malformed HTTP framing, not a codec-only fixture: a token that
    // merely contains `chunked` must not be interpreted as chunked.
    let malformed = tokio::time::timeout(EXCHANGE_BOUND, async {
        let mut stream = connect_external(&connector).await;
        stream
            .write_all(
                b"POST / HTTP/1.1\r\nhost: demo\r\ntransfer-encoding: xchunked\r\n\r\n0\r\n\r\n",
            )
            .await
            .expect("write malformed transfer coding");
        read_http_response(&mut stream).await
    })
    .await
    .expect("malformed-framing response");
    assert_eq!(malformed.0, 400, "malformed transfer coding is rejected");

    // Malformed input at the edge: a request beyond the connector's read
    // buffer is rejected with a typed 413 without contacting the guest.
    let oversized = tokio::time::timeout(EXCHANGE_BOUND, async {
        let mut stream = connect_external(&connector).await;
        let body = vec![b'x'; 20 * 1024];
        let head = format!(
            "POST / HTTP/1.1\r\nhost: demo\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
            body.len()
        );
        stream
            .write_all(head.as_bytes())
            .await
            .expect("write oversized head");
        drop(stream.write_all(&body).await);
        read_http_response(&mut stream).await
    })
    .await
    .expect("oversized exchange");
    assert_eq!(
        oversized.0, 413,
        "an oversized request is rejected at the edge"
    );

    // The same external path fails closed for a route no guest serves: the
    // connector answers with an error rather than hanging or proxying blindly.
    let missing = tokio::time::timeout(EXCHANGE_BOUND, async {
        let mut stream = connect_external(&connector).await;
        stream
            .write_all(b"GET /missing HTTP/1.1\r\nhost: absent\r\nconnection: close\r\n\r\n")
            .await
            .expect("write request");
        read_http_response(&mut stream).await
    })
    .await
    .expect("missing-route response");
    assert_eq!(missing.0, 404, "an unregistered route returns exactly 404");
}

/// The connector must not fall back to plaintext when its certificate
/// material is absent: it fails its startup loudly instead of serving an
/// unauthenticated listener.
// scenario: http-connector/edge-termination-of-http-1-1-over-tls/missing-certificate-material
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires the discovery, store and HTTP connector guests built for wasm32-unknown-unknown"]
async fn http_connector_refuses_to_start_without_tls_material() {
    let _port = SpinePortGuard::acquire();

    let state = StateRoot::new("https-no-material");
    let runtime = runtime_in_state_root(&state);
    // Deliberately no `seed_tls_blob_store`: the store exists and serves, but
    // the connector's `tls-certs` store has no manifests.

    let result = runtime.bootstrap_system_guests(RuntimeConfig {
        start_discovery: true,
        system_guests: vec![
            discovery_descriptor(read_wasm("selium-discovery", "selium_discovery.wasm")),
            store_descriptor(vec!["discovery".to_string()]),
            http_connector_descriptor("127.0.0.1:8444"),
        ],
        state_dir: state.config_dir(),
    });

    let error = result.expect_err("the connector must refuse to boot without TLS material");
    assert!(
        error.to_string().contains("http-connector"),
        "the failure names the connector: {error}"
    );
    let activity: Vec<String> = runtime
        .activity_log()
        .into_iter()
        .map(|event| event.message)
        .collect();
    assert!(
        activity
            .iter()
            .any(|message| message.contains("TLS") && message.contains("http-connector")),
        "the guest's own diagnosis is surfaced: {activity:#?}"
    );
}

fn http_connector_descriptor(listen_addr: &str) -> SystemGuestDescriptor {
    SystemGuestDescriptor {
        name: "http-connector".to_string(),
        module_id: "http-connector-module".to_string(),
        module_bytes: read_wasm("selium-connector-http", "selium_connector_http.wasm"),
        entrypoint: "connector_http".to_string(),
        // A pointer argument keeps the leading `Context` injection intact
        // (the runtime only wires the discovery handle for an empty or
        // pointer-leading argument list).
        arguments: vec![SystemGuestArg::Pointer(listen_addr.as_bytes().to_vec())],
        grants: vec![
            CapabilityGrant::new(
                Capability::Network,
                vec![ResourceSelector::ResourceClass(ResourceClass::TcpListener)],
            ),
            CapabilityGrant::new(
                Capability::Network,
                vec![ResourceSelector::ResourceClass(ResourceClass::TcpStream)],
            ),
            CapabilityGrant::new(
                Capability::SharedMemory,
                vec![ResourceSelector::ResourceClass(ResourceClass::SharedRegion)],
            ),
            CapabilityGrant::new(
                Capability::HostQueue,
                vec![ResourceSelector::ResourceClass(ResourceClass::HostQueue)],
            ),
        ],
        tenant: None,
        discovery_service: false,
        // The host provisions the `sel-http` handler claim; serving guests
        // pin their listeners to it.
        handlers: vec!["sel-http".to_string()],
    }
}

fn http_demo_descriptor() -> SystemGuestDescriptor {
    SystemGuestDescriptor {
        name: "http-demo".to_string(),
        module_id: "http-demo-module".to_string(),
        module_bytes: read_wasm("selium-http-demo", "selium_http_demo.wasm"),
        entrypoint: "http_demo".to_string(),
        arguments: Vec::new(),
        grants: vec![
            CapabilityGrant::new(
                Capability::HostQueue,
                vec![ResourceSelector::ResourceClass(ResourceClass::HostQueue)],
            ),
            CapabilityGrant::new(
                Capability::SharedMemory,
                vec![ResourceSelector::ResourceClass(ResourceClass::SharedRegion)],
            ),
            CapabilityGrant::new(
                Capability::RouteClaim,
                vec![ResourceSelector::Uri("sel:///demo".to_string())],
            ),
            CapabilityGrant::new(
                Capability::RouteClaim,
                vec![ResourceSelector::Uri("sel:///demo/events".to_string())],
            ),
        ],
        tenant: None,
        discovery_service: false,
        handlers: Vec::new(),
    }
}

/// Opens one external TLS connection to the connector (no Selium SDK).
async fn connect_external(connector: &TlsConnector) -> tokio_rustls::client::TlsStream<TcpStream> {
    let stream = TcpStream::connect(CONNECTOR_ADDR)
        .await
        .expect("connect to the connector");
    let server_name = rustls_pki_types::ServerName::try_from("bridge").expect("server name");
    connector
        .connect(server_name, stream)
        .await
        .expect("TLS handshake")
}

/// Reads a chunked HTTP/1.1 response and returns the concatenated body.
async fn read_chunked_response(stream: &mut tokio_rustls::client::TlsStream<TcpStream>) -> Vec<u8> {
    use tokio::io::{AsyncBufReadExt as _, BufReader};

    let mut reader = BufReader::new(stream);
    let mut head = String::new();
    loop {
        let mut line = String::new();
        let read = reader.read_line(&mut line).await.expect("read head line");
        assert_ne!(read, 0, "connection closed before the response head");
        if line == "\r\n" {
            break;
        }
        head.push_str(&line);
    }
    assert!(
        head.to_lowercase().contains("transfer-encoding: chunked"),
        "the connector advertises chunked encoding: {head:?}"
    );

    let mut body = Vec::new();
    loop {
        let mut size_line = String::new();
        reader
            .read_line(&mut size_line)
            .await
            .expect("read chunk size");
        let size = usize::from_str_radix(size_line.trim(), 16).expect("chunk size");
        if size == 0 {
            let mut terminator = String::new();
            drop(reader.read_line(&mut terminator).await);
            break;
        }
        let mut chunk = vec![0u8; size];
        reader.read_exact(&mut chunk).await.expect("read chunk");
        body.extend_from_slice(&chunk);
        let mut crlf = [0u8; 2];
        reader
            .read_exact(&mut crlf)
            .await
            .expect("chunk terminator");
    }
    body
}

/// Reads one HTTP/1.1 response: status code and `content-length` body.
#[expect(clippy::panic, reason = "a malformed response is a hard test failure")]
async fn read_http_response(
    stream: &mut tokio_rustls::client::TlsStream<TcpStream>,
) -> (u16, Vec<u8>) {
    let mut buffer = Vec::new();
    let header_end = loop {
        if let Some(position) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
            break position + 4;
        }
        let mut chunk = [0u8; 1024];
        let read = stream.read(&mut chunk).await.expect("read response");
        assert_ne!(read, 0, "connection closed before the response headers");
        buffer.extend_from_slice(chunk.get(..read).unwrap_or_default());
    };

    let head = String::from_utf8_lossy(buffer.get(..header_end).unwrap_or_default()).to_string();
    let status = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse::<u16>().ok())
        .unwrap_or_else(|| panic!("malformed status line: {head:?}"));
    let content_length = head
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())
                .flatten()
        })
        .unwrap_or(0);

    while buffer.len() < header_end + content_length {
        let mut chunk = [0u8; 1024];
        let read = stream.read(&mut chunk).await.expect("read response body");
        if read == 0 {
            break;
        }
        buffer.extend_from_slice(chunk.get(..read).unwrap_or_default());
    }
    let body = buffer.get(header_end..).unwrap_or_default().to_vec();
    (status, body)
}
