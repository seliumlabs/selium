//! End-to-end pipeline tests: real HTTP bytes through
//! [`handle_connection`] with a mock session factory.
//!
//! These exercise the connector's actual protocol machinery — codec,
//! routing, correlation, windowed backpressure, streaming writes — over
//! in-memory streams, with the transport seam replaced by controllable
//! mocks so ordering and pause/resume behaviour are observable.

use std::{
    collections::HashMap,
    sync::Arc,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};

use selium_abi::ResourceClass;
use selium_connector_http::{
    pipeline::{
        ConnectionConfig, ForwardError, ForwardSession, ReplyEvent, ReplySink, SessionFactory,
        handle_connection,
    },
    resolve::ResolverHandle,
    resolve::test_support::RouteResolver as TestRouteResolver,
};
use selium_proto_http::{HttpHeader, HttpRequest, HttpResponse, HttpStreamItem, HttpTrailer};
use selium_service::ResourceTarget;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::{Mutex, Notify},
};

/// Manual release gate for controlling when a mock session answers.
struct Gate {
    open: AtomicBool,
    notify: Notify,
}

/// Scripted behaviour for one route.
#[derive(Clone)]
enum RouteBehaviour {
    /// Answer with a complete unary response, optionally after a gate.
    Unary {
        response: HttpResponse,
        gate: Option<Arc<Gate>>,
    },
    /// Answer with a streamed response (head/chunks/trailers), optionally
    /// starting after a gate.
    Stream {
        items: Vec<HttpStreamItem>,
        gate: Option<Arc<Gate>>,
    },
}

#[derive(Debug, Clone, PartialEq)]
struct ForwardRecord {
    tag: u64,
    method: String,
    uri: String,
    body: Vec<u8>,
}

#[derive(Default)]
struct MockState {
    routes: Mutex<HashMap<String, RouteBehaviour>>,
    fallback: Mutex<Option<RouteBehaviour>>,
    forwards: Mutex<Vec<ForwardRecord>>,
    connect_failures: AtomicUsize,
}

#[derive(Clone)]
struct MockFactory {
    state: Arc<MockState>,
}

struct MockSession {
    state: Arc<MockState>,
    behaviour: RouteBehaviour,
}

impl Gate {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            open: AtomicBool::new(false),
            notify: Notify::new(),
        })
    }

    fn release(&self) {
        self.open.store(true, Ordering::SeqCst);
        self.notify.notify_waiters();
    }

    async fn wait(&self) {
        loop {
            if self.open.load(Ordering::SeqCst) {
                return;
            }
            let notified = self.notify.notified();
            if self.open.load(Ordering::SeqCst) {
                return;
            }
            notified.await;
        }
    }
}

impl MockFactory {
    fn new() -> Self {
        Self {
            state: Arc::new(MockState::default()),
        }
    }

    async fn set_route(&self, uri: &str, behaviour: RouteBehaviour) {
        self.state
            .routes
            .lock()
            .await
            .insert(uri.to_string(), behaviour);
    }

    async fn set_fallback(&self, behaviour: RouteBehaviour) {
        *self.state.fallback.lock().await = Some(behaviour);
    }

    fn fail_next_connects(&self, count: usize) {
        self.state.connect_failures.store(count, Ordering::SeqCst);
    }

    async fn forwards(&self) -> Vec<ForwardRecord> {
        self.state.forwards.lock().await.clone()
    }
}

impl SessionFactory for MockFactory {
    type Session = MockSession;

    async fn connect(&self, target: &ResourceTarget) -> Result<MockSession, ForwardError> {
        // Scripted connect failures take priority (transport-level error).
        let mut failures = self.state.connect_failures.load(Ordering::SeqCst);
        while failures > 0 {
            match self.state.connect_failures.compare_exchange(
                failures,
                failures - 1,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => return Err(ForwardError::Connect("scripted failure".to_string())),
                Err(actual) => failures = actual,
            }
        }

        let behaviour = self.state.routes.lock().await.get(&target.uri).cloned();
        let behaviour = match behaviour {
            Some(b) => b,
            None => self.state.fallback.lock().await.clone().ok_or_else(|| {
                ForwardError::Connect(format!("no mock route for {}", target.uri))
            })?,
        };

        Ok(MockSession {
            state: self.state.clone(),
            behaviour,
        })
    }
}

impl ForwardSession for MockSession {
    async fn forward<S: ReplySink>(
        self,
        tag: u64,
        request: HttpRequest,
        out: &mut S,
    ) -> Result<(), ForwardError> {
        self.state.forwards.lock().await.push(ForwardRecord {
            tag,
            method: request.method.clone(),
            uri: request.uri.clone(),
            body: request.body.clone(),
        });

        match self.behaviour {
            RouteBehaviour::Unary { response, gate } => {
                if let Some(gate) = gate {
                    gate.wait().await;
                }
                out.emit(ReplyEvent::Complete(response)).await
            }
            RouteBehaviour::Stream { items, gate } => {
                if let Some(gate) = gate {
                    gate.wait().await;
                }
                for item in items {
                    let event = if item.is_head() {
                        ReplyEvent::Head(HttpResponse::new(item.status, item.headers, Vec::new()))
                    } else if item.is_chunk() {
                        ReplyEvent::Chunk(item.data)
                    } else if item.is_trailer() {
                        ReplyEvent::Trailer(HttpTrailer::new(item.name, item.value))
                    } else {
                        ReplyEvent::Fail
                    };
                    out.emit(event).await?;
                }
                out.emit(ReplyEvent::End).await
            }
        }
    }
}

#[tokio::test]
async fn chunked_request_body_is_decoded_before_forwarding() {
    let (mut client, server_side) = tokio::io::duplex(64 * 1024);
    let factory = MockFactory::new();
    factory
        .set_fallback(RouteBehaviour::Unary {
            response: HttpResponse::new(200, vec![], b"received".to_vec()),
            gate: None,
        })
        .await;

    let resolver = seeded_resolver(
        "example.com",
        "/upload",
        target_for("sel://example.com/upload", 4),
    );
    let handle = serve(
        server_side,
        resolver,
        factory.clone(),
        ConnectionConfig::default(),
    )
    .await;

    client
        .write_all(
            b"POST /upload HTTP/1.1\r\nhost: example.com\r\ntransfer-encoding: chunked\r\n\r\n\
              5\r\nhello\r\n7\r\n, world\r\n0\r\n\r\n",
        )
        .await
        .unwrap();

    let wire = read_until(&mut client, "received").await;
    assert!(wire.starts_with(b"HTTP/1.1 200 OK"));

    let forwards = factory.forwards().await;
    assert_eq!(forwards.len(), 1);
    assert_eq!(
        forwards[0].body, b"hello, world",
        "chunked request body must be decoded into the typed request"
    );

    drop(client);
    handle.await.expect("connection handler task");
}

#[tokio::test]
async fn concurrent_connections_are_served_independently() {
    let factory = MockFactory::new();

    let gate = Gate::new();
    factory
        .set_route(
            "sel://a.example/slow",
            RouteBehaviour::Unary {
                response: HttpResponse::new(200, vec![], b"conn-a".to_vec()),
                gate: Some(gate.clone()),
            },
        )
        .await;
    factory
        .set_route(
            "sel://b.example/fast",
            RouteBehaviour::Unary {
                response: HttpResponse::new(200, vec![], b"conn-b".to_vec()),
                gate: None,
            },
        )
        .await;

    let (mut client_a, server_a) = tokio::io::duplex(64 * 1024);
    let (mut client_b, server_b) = tokio::io::duplex(64 * 1024);

    let resolver_a = seeded_resolver("a.example", "/slow", target_for("sel://a.example/slow", 1));
    let resolver_b = seeded_resolver("b.example", "/fast", target_for("sel://b.example/fast", 2));

    let handle_a = serve(
        server_a,
        resolver_a,
        factory.clone(),
        ConnectionConfig::default(),
    )
    .await;
    let handle_b = serve(
        server_b,
        resolver_b,
        factory.clone(),
        ConnectionConfig::default(),
    )
    .await;

    client_a
        .write_all(b"GET /slow HTTP/1.1\r\nhost: a.example\r\n\r\n")
        .await
        .unwrap();
    client_b
        .write_all(b"GET /fast HTTP/1.1\r\nhost: b.example\r\n\r\n")
        .await
        .unwrap();

    // Connection B completes while A is still gated: a parked connection
    // must not block any other connection.
    let wire_b = read_until(&mut client_b, "conn-b").await;
    assert!(wire_b.starts_with(b"HTTP/1.1 200 OK"));

    gate.release();
    let wire_a = read_until(&mut client_a, "conn-a").await;
    assert!(wire_a.starts_with(b"HTTP/1.1 200 OK"));

    drop(client_a);
    drop(client_b);
    handle_a.await.expect("connection handler task a");
    handle_b.await.expect("connection handler task b");
}

#[tokio::test]
async fn connect_failure_yields_502_and_evicts_route() {
    let (mut client, server_side) = tokio::io::duplex(64 * 1024);
    let factory = MockFactory::new();
    factory
        .set_route(
            "sel://example.com/stale",
            RouteBehaviour::Unary {
                response: HttpResponse::new(200, vec![], vec![]),
                gate: None,
            },
        )
        .await;
    factory.fail_next_connects(1);

    let resolver = seeded_resolver(
        "example.com",
        "/stale",
        target_for("sel://example.com/stale", 3),
    );
    let handle = serve(
        server_side,
        resolver.clone(),
        factory.clone(),
        ConnectionConfig::default(),
    )
    .await;

    client
        .write_all(b"GET /stale HTTP/1.1\r\nhost: example.com\r\n\r\n")
        .await
        .unwrap();

    let wire = read_until(&mut client, "Bad Gateway").await;
    let text = String::from_utf8_lossy(&wire);
    assert!(text.starts_with("HTTP/1.1 502 Bad Gateway"), "wire: {text}");

    // The stale route was evicted from the cache.
    assert!(
        !resolver.lock().await.is_cached("example.com", "/stale"),
        "failed route must be evicted so the next request re-resolves"
    );

    drop(client);
    handle.await.expect("connection handler task");
}

fn empty_resolver() -> ResolverHandle {
    Arc::new(tokio::sync::Mutex::new(TestRouteResolver::empty()))
}

#[tokio::test]
async fn full_window_stops_socket_reads_and_resumes_without_loss() {
    // Small duplex buffer + window of one: once the single in-flight
    // request is dispatched, the connector must stop reading the socket —
    // the second pipelined request's bytes stay unread until the first
    // response completes.
    let (mut client, server_side) = tokio::io::duplex(96);
    let factory = MockFactory::new();

    let gate = Gate::new();
    factory
        .set_fallback(RouteBehaviour::Unary {
            response: HttpResponse::new(200, vec![], b"xx".to_vec()),
            gate: Some(gate.clone()),
        })
        .await;

    let resolver = seeded_resolver("example.com", "/bp", target_for("sel://example.com/bp", 9));
    let config = ConnectionConfig {
        max_pipeline: 1,
        reply_capacity: 4,
    };
    let handle = serve(server_side, resolver, factory.clone(), config).await;

    let req = b"GET /bp HTTP/1.1\r\nhost: example.com\r\n\r\n";
    // Write both requests; the duplex buffer (96) holds both (~76 bytes).
    let mut both = Vec::new();
    both.extend_from_slice(req);
    both.extend_from_slice(req);
    client.write_all(&both).await.unwrap();

    // Give the pipeline time to dispatch request 0 and reach the gate.
    for _ in 0..100 {
        tokio::task::yield_now().await;
    }

    // Window full: exactly one request was forwarded; the second request's
    // bytes are still sitting unread in the socket buffer. That is the
    // pause — the connector is not buffering requests without bound.
    assert_eq!(
        factory.forwards().await.len(),
        1,
        "connector must stop reading while the window is full"
    );

    // Release the gate: request 0 completes, the window frees, the
    // connector resumes reading and forwards request 1. No bytes lost.
    gate.release();
    let wire = read_until(&mut client, "HTTP/1.1 200 OK").await;
    assert!(wire.windows(15).any(|w| w == b"HTTP/1.1 200 OK"));

    // Wait for the second request to be forwarded and answered.
    loop {
        if factory.forwards().await.len() == 2 {
            break;
        }
        tokio::task::yield_now().await;
    }
    let forwards = factory.forwards().await;
    assert_eq!(forwards.len(), 2, "second request must not be lost");
    assert_eq!(forwards[0].tag, 0);
    assert_eq!(forwards[1].tag, 1);

    drop(client);
    handle.await.expect("connection handler task");
}

#[tokio::test]
async fn golden_path_http_request_to_typed_forward_and_back() {
    let (mut client, server_side) = tokio::io::duplex(64 * 1024);
    let factory = MockFactory::new();
    factory
        .set_fallback(RouteBehaviour::Unary {
            response: HttpResponse::new(
                200,
                vec![HttpHeader::new(
                    "content-type".to_string(),
                    "text/plain".to_string(),
                )],
                b"hello from the app guest".to_vec(),
            ),
            gate: None,
        })
        .await;

    let resolver = seeded_resolver(
        "example.com",
        "/api",
        target_for("sel://example.com/api", 7),
    );
    let handle = serve(
        server_side,
        resolver,
        factory.clone(),
        ConnectionConfig::default(),
    )
    .await;

    client
        .write_all(b"GET /api HTTP/1.1\r\nhost: example.com\r\n\r\n")
        .await
        .unwrap();

    let wire = read_until(&mut client, "hello from the app guest").await;
    let text = String::from_utf8_lossy(&wire);
    assert!(text.starts_with("HTTP/1.1 200 OK"), "wire: {text}");
    assert!(text.contains("content-type: text/plain"));
    assert!(text.contains("Content-Length: 24"));

    // The typed forwarding seam saw the parsed request.
    let forwards = factory.forwards().await;
    assert_eq!(forwards.len(), 1);
    assert_eq!(forwards[0].method, "GET");
    assert_eq!(forwards[0].uri, "/api");
    assert!(forwards[0].body.is_empty());

    drop(client);
    handle.await.expect("connection handler task");
}

#[tokio::test]
async fn oversized_request_gets_typed_413() {
    let (mut client, server_side) = tokio::io::duplex(128 * 1024);
    let factory = MockFactory::new();
    factory
        .set_fallback(RouteBehaviour::Unary {
            response: HttpResponse::new(200, vec![], b"never".to_vec()),
            gate: None,
        })
        .await;

    let resolver = seeded_resolver(
        "example.com",
        "/big",
        target_for("sel://example.com/big", 6),
    );
    let handle = serve(
        server_side,
        resolver,
        factory.clone(),
        ConnectionConfig::default(),
    )
    .await;

    // Claim a body far beyond the edge limit and stream bytes until the
    // codec trips the typed oversize condition.
    client
        .write_all(b"PUT /big HTTP/1.1\r\nhost: example.com\r\ncontent-length: 999999\r\n\r\n")
        .await
        .unwrap();
    let blob = vec![b'x'; 32 * 1024];
    client.write_all(&blob).await.unwrap();

    let wire = read_until(&mut client, "Payload Too Large").await;
    let text = String::from_utf8_lossy(&wire);
    assert!(
        text.starts_with("HTTP/1.1 413 Payload Too Large"),
        "wire: {text}"
    );

    // The oversized request was never forwarded.
    assert!(factory.forwards().await.is_empty());

    drop(client);
    handle.await.expect("connection handler task");
}

#[tokio::test]
async fn pipelined_requests_get_distinct_tags_and_ordered_responses() {
    let (mut client, server_side) = tokio::io::duplex(64 * 1024);
    let factory = MockFactory::new();

    // Request 0 is held by a gate; request 1 answers immediately. Even so,
    // responses MUST reach the wire in request order.
    let gate = Gate::new();
    factory
        .set_route(
            "sel://example.com/slow",
            RouteBehaviour::Unary {
                response: HttpResponse::new(200, vec![], b"slow-body".to_vec()),
                gate: Some(gate.clone()),
            },
        )
        .await;
    factory
        .set_route(
            "sel://example.com/fast",
            RouteBehaviour::Unary {
                response: HttpResponse::new(200, vec![], b"fast-body".to_vec()),
                gate: None,
            },
        )
        .await;

    let mut resolver_map = HashMap::new();
    resolver_map.insert("/slow".to_string(), target_for("sel://example.com/slow", 1));
    resolver_map.insert("/fast".to_string(), target_for("sel://example.com/fast", 2));
    let resolver = Arc::new(tokio::sync::Mutex::new(TestRouteResolver::with_routes(
        "example.com",
        resolver_map,
    )));

    let handle = serve(
        server_side,
        resolver,
        factory.clone(),
        ConnectionConfig::default(),
    )
    .await;

    // Two pipelined requests in one write.
    client
        .write_all(
            b"GET /slow HTTP/1.1\r\nhost: example.com\r\n\r\n\
              GET /fast HTTP/1.1\r\nhost: example.com\r\n\r\n",
        )
        .await
        .unwrap();

    // Let the pipeline dispatch both requests and complete the fast one.
    for _ in 0..50 {
        tokio::task::yield_now().await;
    }

    // Both were forwarded, each with a distinct connection-level tag.
    let forwards = factory.forwards().await;
    assert_eq!(forwards.len(), 2);
    let tags: Vec<u64> = forwards.iter().map(|f| f.tag).collect();
    assert_eq!(tags[0], 0);
    assert_eq!(tags[1], 1);
    assert_ne!(
        tags[0], tags[1],
        "each typed request carries a distinct tag"
    );

    // The fast response is done but MUST NOT be on the wire before the
    // slow one (HTTP/1.1 response ordering).
    let mut probe = [0u8; 256];
    let n = tokio::time::timeout(
        std::time::Duration::from_millis(100),
        client.read(&mut probe),
    )
    .await;
    let seen_so_far = match n {
        Ok(Ok(k)) => String::from_utf8_lossy(&probe[..k]).into_owned(),
        _ => String::new(),
    };
    assert!(
        !seen_so_far.contains("fast-body"),
        "out-of-order response leaked to the wire: {seen_so_far}"
    );

    // Release the slow request: now both responses arrive, in request order.
    gate.release();
    let wire = read_until(&mut client, "fast-body").await;
    let text = String::from_utf8_lossy(&wire);
    let slow_pos = text.find("slow-body").expect("slow body on wire");
    let fast_pos = text.find("fast-body").expect("fast body on wire");
    assert!(
        slow_pos < fast_pos,
        "responses must be emitted in request order: {text}"
    );

    drop(client);
    handle.await.expect("connection handler task");
}

/// Reads from `client` until the buffer contains `marker` (or EOF).
#[expect(clippy::panic, reason = "test helper")]
async fn read_until(client: &mut tokio::io::DuplexStream, marker: &str) -> Vec<u8> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        if std::str::from_utf8(&buf)
            .map(|s| s.contains(marker))
            .unwrap_or(false)
        {
            return buf;
        }
        match tokio::time::timeout(std::time::Duration::from_secs(5), client.read(&mut chunk)).await
        {
            Ok(Ok(0)) => return buf,
            Ok(Ok(n)) => buf.extend_from_slice(chunk.get(..n).unwrap_or_default()),
            Ok(Err(e)) => panic!("client read error: {e}"),
            Err(_) => panic!(
                "timed out waiting for marker {marker:?}; got so far: {:?}",
                String::from_utf8_lossy(&buf)
            ),
        }
    }
}

fn seeded_resolver(host: &str, path: &str, target: ResourceTarget) -> ResolverHandle {
    Arc::new(tokio::sync::Mutex::new(
        TestRouteResolver::with_cached_route(host, path, target),
    ))
}

/// Runs `handle_connection` against one half of a duplex pair.
async fn serve(
    server_side: tokio::io::DuplexStream,
    resolver: ResolverHandle,
    factory: MockFactory,
    config: ConnectionConfig,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(handle_connection(server_side, resolver, factory, config))
}

#[tokio::test]
async fn streamed_response_is_written_as_chunked_encoding() {
    let (mut client, server_side) = tokio::io::duplex(64 * 1024);
    let factory = MockFactory::new();
    factory
        .set_fallback(RouteBehaviour::Stream {
            items: vec![
                HttpStreamItem::head(
                    200,
                    vec![HttpHeader::new(
                        "content-type".to_string(),
                        "text/event-stream".to_string(),
                    )],
                ),
                HttpStreamItem::chunk(b"data: one\n\n".to_vec()),
                HttpStreamItem::chunk(b"data: two\n\n".to_vec()),
                HttpStreamItem::trailer("x-events", "2"),
            ],
            gate: None,
        })
        .await;

    let resolver = seeded_resolver(
        "example.com",
        "/events",
        target_for("sel://example.com/events", 5),
    );
    let handle = serve(
        server_side,
        resolver,
        factory.clone(),
        ConnectionConfig::default(),
    )
    .await;

    client
        .write_all(b"GET /events HTTP/1.1\r\nhost: example.com\r\n\r\n")
        .await
        .unwrap();

    let wire = read_until(&mut client, "x-events: 2").await;
    let text = String::from_utf8_lossy(&wire);

    assert!(text.starts_with("HTTP/1.1 200 OK"), "wire: {text}");
    assert!(
        text.contains("Transfer-Encoding: chunked"),
        "streamed responses must use chunked framing: {text}"
    );
    assert!(text.contains("content-type: text/event-stream"));
    // Chunked body: size lines, data, terminating zero chunk + trailer.
    assert!(text.contains("b\r\ndata: one\n\n\r\n"), "wire: {text}");
    assert!(text.contains("b\r\ndata: two\n\n\r\n"), "wire: {text}");
    assert!(text.ends_with("0\r\nx-events: 2\r\n\r\n"), "wire: {text}");

    drop(client);
    handle.await.expect("connection handler task");
}

fn target_for(uri: &str, id: u64) -> ResourceTarget {
    ResourceTarget {
        uri: uri.to_string(),
        host_id: String::new(),
        resource_id: id,
        interface: None,
        tenant: None,
        class: ResourceClass::HostQueue,
        labels: Vec::new(),
    }
}

#[tokio::test]
async fn unmatched_route_responds_404_without_forwarding() {
    let (mut client, server_side) = tokio::io::duplex(64 * 1024);
    let factory = MockFactory::new();
    factory
        .set_fallback(RouteBehaviour::Unary {
            response: HttpResponse::new(200, vec![], b"should not appear".to_vec()),
            gate: None,
        })
        .await;

    let handle = serve(
        server_side,
        empty_resolver(),
        factory.clone(),
        ConnectionConfig::default(),
    )
    .await;

    client
        .write_all(b"GET /nope HTTP/1.1\r\nhost: unknown.example\r\n\r\n")
        .await
        .unwrap();

    let wire = read_until(&mut client, "Not Found").await;
    let text = String::from_utf8_lossy(&wire);
    assert!(text.starts_with("HTTP/1.1 404 Not Found"), "wire: {text}");

    // No app guest was contacted.
    assert!(factory.forwards().await.is_empty());

    drop(client);
    handle.await.expect("connection handler task");
}
