//! QUIC handshake spike: a real TLS 1.3 handshake through the connector's
//! production [`build_endpoint`] seam.
//!
//! The production [`QuicUdpSocket`] and [`ConnectorRuntime`] are WASM/shm
//! adapters that need the guest hostcalls, so they cannot drive a real
//! handshake in a native test. This test therefore substitutes two native
//! test doubles — a `tokio` UDP socket adapter and a `tokio` runtime — and
//! runs [`build_endpoint`] exactly as the entrypoint does, then completes a
//! handshake against a host-side quinn client and round-trips bytes on a
//! bidirectional stream.
//!
//! The shm adapter/runtime types themselves are compile-verified against
//! quinn's trait bounds in the crate's `tests` module (`cargo check`), and
//! their wire behaviour is exercised end-to-end in the runtime substrate
//! tests once the guest is built for wasm32.

use std::{
    future::Future,
    io::{self, IoSliceMut},
    net::SocketAddr,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use parking_lot::Mutex;
use quinn::{
    ClientConfig, ServerConfig,
    udp::{RecvMeta, Transmit},
};
use selium_connector_quic::{
    MAX_CONCURRENT_BIDI_STREAMS, build_endpoint, handle_connection,
    identity::{ClientAnchorSet, build_server_config},
    rate::{AdmissionLimiter, refuse_stream},
    resolve::RouteResolver,
    runtime::{ConnectorRuntime, ConnectorTimer},
    sni_of,
    udp_adapter::QuicUdpSocket,
};
use selium_guest::time::Instant;

const CERT_DER: &[u8] = include_bytes!("fixtures/cert.der");
const CLIENT_CERT_DER: &[u8] = include_bytes!("fixtures/client_cert.der");
const CLIENT_KEY_DER: &[u8] = include_bytes!("fixtures/client_key.der");
const KEY_DER: &[u8] = include_bytes!("fixtures/key.der");

/// A native test-only `quinn::AsyncUdpSocket` over `tokio::net::UdpSocket`.
struct TokioUdpSocket {
    inner: Arc<tokio::net::UdpSocket>,
    buf: Mutex<Vec<u8>>,
}

struct TokioUdpPoller {
    socket: Arc<TokioUdpSocket>,
    writable: Option<Pin<Box<dyn Future<Output = io::Result<()>> + Send + Sync>>>,
}

/// A native test-only `quinn::Runtime` over the tokio executor.
#[derive(Debug, Default)]
struct TokioRuntime;

struct TokioTimer {
    deadline: std::time::Instant,
    sleep: Option<Pin<Box<tokio::time::Sleep>>>,
}

impl quinn::AsyncUdpSocket for TokioUdpSocket {
    fn create_io_poller(self: Arc<Self>) -> Pin<Box<dyn quinn::UdpPoller>> {
        Box::pin(TokioUdpPoller {
            socket: self,
            writable: None,
        })
    }

    fn try_send(&self, transmit: &Transmit) -> io::Result<()> {
        self.inner
            .try_send_to(transmit.contents, transmit.destination)
            .map(|_| ())
    }

    fn poll_recv(
        &self,
        cx: &mut Context<'_>,
        bufs: &mut [IoSliceMut<'_>],
        meta: &mut [RecvMeta],
    ) -> Poll<io::Result<usize>> {
        let mut guard = self.buf.lock();
        let buf = &mut *guard;
        buf.resize(65536, 0);
        let mut read_buf = tokio::io::ReadBuf::new(buf);
        match self.inner.poll_recv_from(cx, &mut read_buf) {
            Poll::Ready(Ok(addr)) => {
                let n = read_buf.filled().len();
                if let (Some(dst), Some(meta_slot)) = (bufs.first_mut(), meta.first_mut()) {
                    if let Some(source) = read_buf.filled().get(..n)
                        && let Some(target) = dst.get_mut(..n)
                    {
                        target.copy_from_slice(source);
                    }
                    *meta_slot = RecvMeta {
                        addr,
                        len: n,
                        stride: n,
                        ecn: None,
                        dst_ip: None,
                    };
                    Poll::Ready(Ok(1))
                } else {
                    Poll::Ready(Ok(0))
                }
            }
            Poll::Ready(Err(e)) => Poll::Ready(Err(e)),
            Poll::Pending => Poll::Pending,
        }
    }

    fn local_addr(&self) -> io::Result<SocketAddr> {
        self.inner.local_addr()
    }

    fn max_transmit_segments(&self) -> usize {
        1
    }

    fn max_receive_segments(&self) -> usize {
        1
    }

    fn may_fragment(&self) -> bool {
        false
    }
}

impl std::fmt::Debug for TokioUdpSocket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokioUdpSocket").finish_non_exhaustive()
    }
}

impl quinn::UdpPoller for TokioUdpPoller {
    fn poll_writable(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        if self.writable.is_none() {
            let socket = self.socket.clone();
            self.writable = Some(Box::pin(async move { socket.inner.writable().await }));
        }
        let future = self.writable.as_mut().expect("writable future present");
        match Pin::new(future).poll(cx) {
            Poll::Ready(result) => {
                self.writable = None;
                Poll::Ready(result)
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

impl std::fmt::Debug for TokioUdpPoller {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokioUdpPoller").finish_non_exhaustive()
    }
}

impl quinn::Runtime for TokioRuntime {
    fn new_timer(&self, deadline: std::time::Instant) -> Pin<Box<dyn quinn::AsyncTimer>> {
        Box::pin(TokioTimer {
            deadline,
            sleep: None,
        })
    }

    fn spawn(&self, future: Pin<Box<dyn Future<Output = ()> + Send>>) {
        tokio::spawn(future);
    }

    fn wrap_udp_socket(
        &self,
        _: std::net::UdpSocket,
    ) -> io::Result<Arc<dyn quinn::AsyncUdpSocket>> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "use new_with_abstract_socket",
        ))
    }

    fn now(&self) -> std::time::Instant {
        std::time::Instant::now()
    }
}

impl quinn::AsyncTimer for TokioTimer {
    fn reset(self: Pin<&mut Self>, deadline: std::time::Instant) {
        let this = self.get_mut();
        this.deadline = deadline;
        this.sleep = None;
    }

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        let this = self.get_mut();
        if std::time::Instant::now() >= this.deadline {
            this.sleep = None;
            return Poll::Ready(());
        }
        if this.sleep.is_none() {
            this.sleep = Some(Box::pin(tokio::time::sleep_until(
                tokio::time::Instant::from_std(this.deadline),
            )));
        }
        if let Some(sleep) = this.sleep.as_mut() {
            match Future::poll(sleep.as_mut(), cx) {
                Poll::Ready(()) => {
                    this.sleep = None;
                    Poll::Ready(())
                }
                Poll::Pending => Poll::Pending,
            }
        } else {
            Poll::Pending
        }
    }
}

impl std::fmt::Debug for TokioTimer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokioTimer")
            .field("deadline", &self.deadline)
            .finish()
    }
}

/// A client presenting no certificate (or one outside the anchors) fails the
/// handshake before any guest is contacted: `incoming.await` errors.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn certless_client_handshake_is_refused() {
    let (server_config, _anchors) = mtls_server_config();
    let cert = quinn::rustls::pki_types::CertificateDer::from(CERT_DER.to_vec());

    let server_socket = tokio::net::UdpSocket::bind("127.0.0.1:0")
        .await
        .expect("bind server socket");
    let server_addr = server_socket.local_addr().expect("server addr");
    let server_socket = TokioUdpSocket {
        inner: Arc::new(server_socket),
        buf: Mutex::new(vec![0u8; 65536]),
    };
    let endpoint = build_endpoint(
        Arc::new(server_socket),
        Arc::new(TokioRuntime),
        Some(server_config),
    )
    .expect("build server endpoint");

    // A client trusting the server but presenting NO client certificate.
    let client_socket = tokio::net::UdpSocket::bind("127.0.0.1:0")
        .await
        .expect("bind client socket");
    let client_socket = TokioUdpSocket {
        inner: Arc::new(client_socket),
        buf: Mutex::new(vec![0u8; 65536]),
    };
    let mut client_endpoint = build_endpoint(Arc::new(client_socket), Arc::new(TokioRuntime), None)
        .expect("build client endpoint");
    let mut roots = quinn::rustls::RootCertStore::empty();
    roots.add(cert).expect("trust server cert");
    client_endpoint.set_default_client_config(
        ClientConfig::with_root_certificates(Arc::new(roots)).expect("client config"),
    );

    let client_task = {
        let ep = client_endpoint.clone();
        tokio::spawn(async move {
            ep.connect(server_addr, "localhost")
                .expect("connect")
                .await
                .expect("client connection (fails)")
        })
    };

    // The server-side handshake must not complete for a certless client.
    let mut refused = false;
    if let Some(incoming) =
        tokio::time::timeout(std::time::Duration::from_secs(5), endpoint.accept())
            .await
            .expect("accept within timeout")
    {
        refused = incoming.await.is_err();
    }

    assert!(
        refused,
        "certless client must be refused before any guest contact"
    );

    // The client task must observe the connection failure (the endpoint drop
    // below unblocks it either way).
    drop(tokio::time::timeout(std::time::Duration::from_secs(5), client_task).await);

    drop(endpoint);
    drop(client_endpoint);
}

/// Builds a client config that trusts the server's certificate and presents
/// the embedded client certificate for client authentication.
fn client_config_with_certificate(
    server_cert: quinn::rustls::pki_types::CertificateDer<'static>,
) -> ClientConfig {
    use quinn::crypto::rustls::QuicClientConfig;
    use quinn::rustls::{
        RootCertStore,
        client::WebPkiServerVerifier,
        crypto::ring::default_provider,
        pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer},
        version::TLS13,
    };

    let mut roots = RootCertStore::empty();
    roots.add(server_cert).expect("trust server cert");

    let client_cert = CertificateDer::from(CLIENT_CERT_DER.to_vec());
    let client_key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(CLIENT_KEY_DER.to_vec()));

    let rustls_client =
        quinn::rustls::ClientConfig::builder_with_provider(Arc::new(default_provider()))
            .with_protocol_versions(&[&TLS13])
            .expect("ring provider supports TLS 1.3")
            .dangerous()
            .with_custom_certificate_verifier(
                WebPkiServerVerifier::builder_with_provider(
                    Arc::new(roots),
                    Arc::new(default_provider()),
                )
                .build()
                .expect("server verifier"),
            )
            .with_client_auth_cert(vec![client_cert], client_key)
            .expect("client auth cert");

    ClientConfig::new(Arc::new(
        QuicClientConfig::try_from(rustls_client).expect("quic client"),
    ))
}

/// Task 6.1: the server transport config caps concurrent bidirectional
/// streams per connection. A client opening exactly the cap succeeds; the
/// next stream is refused (blocked on stream credit the connector never
/// grants), so no per-stream channel would be allocated for it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_bidi_streams_beyond_the_cap_are_refused() {
    let cert = quinn::rustls::pki_types::CertificateDer::from(CERT_DER.to_vec());
    let key = quinn::rustls::pki_types::PrivateKeyDer::Pkcs8(
        quinn::rustls::pki_types::PrivatePkcs8KeyDer::from(KEY_DER.to_vec()),
    );
    let server_config = build_server_config(vec![cert], key, None).expect("server config");

    let server_socket = tokio::net::UdpSocket::bind("127.0.0.1:0")
        .await
        .expect("bind server socket");
    let server_addr = server_socket.local_addr().expect("server addr");
    let server_socket = TokioUdpSocket {
        inner: Arc::new(server_socket),
        buf: Mutex::new(vec![0u8; 65536]),
    };
    let endpoint = build_endpoint(
        Arc::new(server_socket),
        Arc::new(TokioRuntime),
        Some(server_config),
    )
    .expect("build server endpoint");

    let client_socket = tokio::net::UdpSocket::bind("127.0.0.1:0")
        .await
        .expect("bind client socket");
    let client_socket = TokioUdpSocket {
        inner: Arc::new(client_socket),
        buf: Mutex::new(vec![0u8; 65536]),
    };
    let mut client_endpoint = build_endpoint(Arc::new(client_socket), Arc::new(TokioRuntime), None)
        .expect("build client endpoint");
    let mut roots = quinn::rustls::RootCertStore::empty();
    roots
        .add(quinn::rustls::pki_types::CertificateDer::from(
            CERT_DER.to_vec(),
        ))
        .expect("trust server cert");
    client_endpoint.set_default_client_config(
        ClientConfig::with_root_certificates(Arc::new(roots)).expect("client config"),
    );

    let client_task = {
        let ep = client_endpoint.clone();
        tokio::spawn(async move {
            ep.connect(server_addr, "localhost")
                .expect("connect")
                .await
                .expect("client connection")
        })
    };
    let _server_conn = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let incoming = endpoint.accept().await.expect("server incoming");
        incoming.await.expect("server handshake")
    })
    .await
    .expect("handshake completes");
    let client_conn = client_task.await.expect("client task");

    // Open exactly the advertised cap: each succeeds immediately.
    let cap = MAX_CONCURRENT_BIDI_STREAMS as usize;
    let mut opened = Vec::with_capacity(cap);
    for _ in 0..cap {
        opened.push(client_conn.open_bi().await.expect("within cap"));
    }

    // The next stream must be refused (blocked on stream credit) rather than
    // admitted: it never completes within a short window.
    let overflow =
        tokio::time::timeout(std::time::Duration::from_secs(2), client_conn.open_bi()).await;
    assert!(
        overflow.is_err(),
        "the {cap}+1th concurrent stream must be refused before allocation"
    );

    drop(opened);
    drop(client_conn);
    drop(endpoint);
    drop(client_endpoint);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn handshake_completes_and_relays_a_stream() {
    let (server_config, cert) = server_config();

    // Server endpoint over a loopback tokio UDP socket.
    let server_socket = tokio::net::UdpSocket::bind("127.0.0.1:0")
        .await
        .expect("bind server socket");
    let server_addr = server_socket.local_addr().expect("server addr");
    let server_socket = TokioUdpSocket {
        inner: Arc::new(server_socket),
        buf: Mutex::new(vec![0u8; 65536]),
    };
    let endpoint = build_endpoint(
        Arc::new(server_socket),
        Arc::new(TokioRuntime),
        Some(server_config),
    )
    .expect("build server endpoint");

    // Client endpoint with the self-signed cert trusted.
    let client_socket = tokio::net::UdpSocket::bind("127.0.0.1:0")
        .await
        .expect("bind client socket");
    let client_socket = TokioUdpSocket {
        inner: Arc::new(client_socket),
        buf: Mutex::new(vec![0u8; 65536]),
    };
    let mut client_endpoint = build_endpoint(Arc::new(client_socket), Arc::new(TokioRuntime), None)
        .expect("build client endpoint");

    let mut roots = quinn::rustls::RootCertStore::empty();
    roots.add(cert).expect("add root cert");
    let client_config =
        ClientConfig::with_root_certificates(Arc::new(roots)).expect("client config");
    client_endpoint.set_default_client_config(client_config);

    // Drive both sides concurrently: the client handshake runs in a spawned
    // task while the server accepts.
    let client_task = {
        let ep = client_endpoint.clone();
        tokio::spawn(async move {
            ep.connect(server_addr, "localhost")
                .expect("client connect")
                .await
                .expect("client connection")
        })
    };

    let server_conn = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let incoming = endpoint.accept().await.expect("server incoming");
        incoming
            .await
            .expect("server handshake completes (incoming.await yields a connection)")
    })
    .await
    .expect("handshake completed within timeout");

    let client_conn = client_task.await.expect("client task");

    // Drive the client stream concurrently: open, write "ping", read "pong".
    let client_stream_task = {
        let client_conn = client_conn.clone();
        tokio::spawn(async move {
            let (mut send, mut recv) = client_conn.open_bi().await.expect("client open_bi");
            send.write_all(b"ping").await.expect("client write");
            send.finish().expect("client finish");

            let mut buf = [0u8; 4];
            recv.read_exact(&mut buf).await.expect("client read");
            assert_eq!(&buf, b"pong");
        })
    };

    let (mut send, mut recv) = server_conn
        .accept_bi()
        .await
        .expect("server accepts bidirectional stream");

    let mut buf = [0u8; 4];
    recv.read_exact(&mut buf).await.expect("server read");
    assert_eq!(&buf, b"ping");

    send.write_all(b"pong").await.expect("server write");
    send.finish().expect("server finish");

    client_stream_task.await.expect("client stream task");

    drop(client_conn);
    drop(endpoint);
    drop(client_endpoint);
}

/// mTLS is opt-in: with no trust anchors configured, the connector serves
/// without client authentication, so a certless client completes the
/// handshake (restoring pre-mTLS behaviour for non-bridge deployments).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mtls_off_accepts_certless_client() {
    let cert = quinn::rustls::pki_types::CertificateDer::from(CERT_DER.to_vec());
    let key = quinn::rustls::pki_types::PrivateKeyDer::Pkcs8(
        quinn::rustls::pki_types::PrivatePkcs8KeyDer::from(KEY_DER.to_vec()),
    );
    let server_config = build_server_config(vec![cert], key, None).expect("no-mtls server config");

    let server_socket = tokio::net::UdpSocket::bind("127.0.0.1:0")
        .await
        .expect("bind server socket");
    let server_addr = server_socket.local_addr().expect("server addr");
    let server_socket = TokioUdpSocket {
        inner: Arc::new(server_socket),
        buf: Mutex::new(vec![0u8; 65536]),
    };
    let endpoint = build_endpoint(
        Arc::new(server_socket),
        Arc::new(TokioRuntime),
        Some(server_config),
    )
    .expect("build server endpoint");

    // A client trusting the server but presenting NO client certificate.
    let client_socket = tokio::net::UdpSocket::bind("127.0.0.1:0")
        .await
        .expect("bind client socket");
    let client_socket = TokioUdpSocket {
        inner: Arc::new(client_socket),
        buf: Mutex::new(vec![0u8; 65536]),
    };
    let mut client_endpoint = build_endpoint(Arc::new(client_socket), Arc::new(TokioRuntime), None)
        .expect("build client endpoint");
    let mut roots = quinn::rustls::RootCertStore::empty();
    roots
        .add(quinn::rustls::pki_types::CertificateDer::from(
            CERT_DER.to_vec(),
        ))
        .expect("trust server cert");
    client_endpoint.set_default_client_config(
        ClientConfig::with_root_certificates(Arc::new(roots)).expect("client config"),
    );

    let client_task = {
        let ep = client_endpoint.clone();
        tokio::spawn(async move {
            ep.connect(server_addr, "localhost")
                .expect("connect")
                .await
                .expect("certless client completes handshake with mTLS off")
        })
    };

    let _server_conn = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let incoming = endpoint.accept().await.expect("server incoming");
        incoming
            .await
            .expect("server handshake completes with mTLS off")
    })
    .await
    .expect("handshake completed within timeout");
    drop(client_task.await.expect("client task"));

    drop(endpoint);
    drop(client_endpoint);
}

/// Builds an mTLS server config (mandatory client auth against the test
/// anchor) plus the tenant anchor set it was built from.
fn mtls_server_config() -> (ServerConfig, ClientAnchorSet) {
    let cert = quinn::rustls::pki_types::CertificateDer::from(CERT_DER.to_vec());
    let key = quinn::rustls::pki_types::PrivateKeyDer::Pkcs8(
        quinn::rustls::pki_types::PrivatePkcs8KeyDer::from(KEY_DER.to_vec()),
    );
    let anchors = test_anchor_set();
    let config = build_server_config(vec![cert], key, Some(&anchors)).expect("mtls server config");
    (config, anchors)
}

/// The production shm adapter + guest runtime satisfy quinn's trait bounds.
///
/// This is the compile-level verification for the wasm-only types (they cannot
/// drive a real handshake natively); the wire behaviour is exercised by the
/// runtime substrate tests once the guest is built for wasm32.
#[test]
fn production_adapter_types_satisfy_quinn_trait_bounds() {
    fn assert_udp<T: quinn::AsyncUdpSocket>() {}
    fn assert_runtime<T: quinn::Runtime>() {}
    fn assert_timer<T: quinn::AsyncTimer>() {}
    assert_udp::<QuicUdpSocket>();
    assert_runtime::<ConnectorRuntime>();
    assert_timer::<ConnectorTimer>();
}

/// Task 6.2: a burst above the admission rate is refused with a distinct
/// stream reset while the connection itself stays up for further streams.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn refused_stream_resets_but_connection_stays_up() {
    let (server_config, cert) = server_config();

    let server_socket = tokio::net::UdpSocket::bind("127.0.0.1:0")
        .await
        .expect("bind server socket");
    let server_addr = server_socket.local_addr().expect("server addr");
    let server_socket = TokioUdpSocket {
        inner: Arc::new(server_socket),
        buf: Mutex::new(vec![0u8; 65536]),
    };
    let endpoint = build_endpoint(
        Arc::new(server_socket),
        Arc::new(TokioRuntime),
        Some(server_config),
    )
    .expect("build server endpoint");

    let client_socket = tokio::net::UdpSocket::bind("127.0.0.1:0")
        .await
        .expect("bind client socket");
    let client_socket = TokioUdpSocket {
        inner: Arc::new(client_socket),
        buf: Mutex::new(vec![0u8; 65536]),
    };
    let mut client_endpoint = build_endpoint(Arc::new(client_socket), Arc::new(TokioRuntime), None)
        .expect("build client endpoint");
    let mut roots = quinn::rustls::RootCertStore::empty();
    roots.add(cert).expect("trust server cert");
    client_endpoint.set_default_client_config(
        ClientConfig::with_root_certificates(Arc::new(roots)).expect("client config"),
    );

    let client_task = {
        let ep = client_endpoint.clone();
        tokio::spawn(async move {
            ep.connect(server_addr, "localhost")
                .expect("connect")
                .await
                .expect("client connection")
        })
    };
    let server_conn = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let incoming = endpoint.accept().await.expect("server incoming");
        incoming.await.expect("server handshake")
    })
    .await
    .expect("handshake completes");
    let client_conn = client_task.await.expect("client task");

    // A burst-1 limiter admits one stream and deprives the rest (no refill).
    let limiter = AdmissionLimiter::new(0, 1);
    let now = Instant::from_nanos(0);
    assert!(limiter.allow("acme", now).await, "first stream admitted");

    // The client opens a stream and writes, so the server accepts it.
    let client_stream_task = {
        let client_conn = client_conn.clone();
        tokio::spawn(async move {
            let (mut send, mut recv) = client_conn.open_bi().await.expect("open_bi");
            send.write_all(b"x").await.expect("client write");
            send.finish().expect("client finish");
            // The refused stream surfaces as a peer reset (distinct code).
            let mut buf = [0u8; 1];
            match recv.read(&mut buf).await {
                Err(quinn::ReadError::Reset(code)) => {
                    assert_eq!(
                        code.into_inner() as u32,
                        selium_connector_quic::ADMISSION_REFUSED_ERROR_CODE
                    );
                }
                other => panic!("expected a peer reset with the admission code, got {other:?}"),
            }
        })
    };

    let (send, recv) = server_conn
        .accept_bi()
        .await
        .expect("server accepts bidirectional stream");
    // The second admission is deprived: refuse before allocation.
    assert!(
        !limiter.allow("acme", now).await,
        "burst above the rate refused"
    );
    refuse_stream(send, recv);

    client_stream_task.await.expect("client stream task");

    // The connection stays up: a fresh stream can still be opened.
    let _probe = client_conn.open_bi().await.expect("connection stays up");

    drop(client_conn);
    drop(endpoint);
    drop(client_endpoint);
}

/// Builds the server config from the embedded self-signed test certificate,
/// returning the certificate so the client can trust it.
fn server_config() -> (
    ServerConfig,
    quinn::rustls::pki_types::CertificateDer<'static>,
) {
    let cert = quinn::rustls::pki_types::CertificateDer::from(CERT_DER.to_vec());
    let key = quinn::rustls::pki_types::PrivateKeyDer::Pkcs8(
        quinn::rustls::pki_types::PrivatePkcs8KeyDer::from(KEY_DER.to_vec()),
    );
    let config = ServerConfig::with_single_cert(vec![cert.clone()], key).expect("server config");
    (config, cert)
}

/// A test anchor set trusting the embedded client certificate as tenant
/// "acme"'s trust anchor (self-signed: the anchor *is* the client cert).
fn test_anchor_set() -> ClientAnchorSet {
    use quinn::rustls::pki_types::CertificateDer;

    let cert = CertificateDer::from(CLIENT_CERT_DER.to_vec());
    ClientAnchorSet::new(vec![("acme".to_string(), cert)]).expect("build test anchor set")
}

/// A trusted client presenting the configured certificate completes the
/// handshake, and the server derives the authenticated identity (tenant +
/// fingerprint) from the connection — the identity later attached as handoff
/// metadata.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn trusted_client_handshake_derives_identity() {
    let (server_config, anchors) = mtls_server_config();
    let cert = quinn::rustls::pki_types::CertificateDer::from(CERT_DER.to_vec());

    let server_socket = tokio::net::UdpSocket::bind("127.0.0.1:0")
        .await
        .expect("bind server socket");
    let server_addr = server_socket.local_addr().expect("server addr");
    let server_socket = TokioUdpSocket {
        inner: Arc::new(server_socket),
        buf: Mutex::new(vec![0u8; 65536]),
    };
    let endpoint = build_endpoint(
        Arc::new(server_socket),
        Arc::new(TokioRuntime),
        Some(server_config),
    )
    .expect("build server endpoint");

    let client_socket = tokio::net::UdpSocket::bind("127.0.0.1:0")
        .await
        .expect("bind client socket");
    let client_socket = TokioUdpSocket {
        inner: Arc::new(client_socket),
        buf: Mutex::new(vec![0u8; 65536]),
    };
    let mut client_endpoint = build_endpoint(Arc::new(client_socket), Arc::new(TokioRuntime), None)
        .expect("build client endpoint");
    client_endpoint.set_default_client_config(client_config_with_certificate(cert));

    let client_task = {
        let ep = client_endpoint.clone();
        tokio::spawn(async move {
            ep.connect(server_addr, "localhost")
                .expect("client connect")
                .await
                .expect("trusted client connection")
        })
    };
    let server_conn = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let incoming = endpoint.accept().await.expect("server incoming");
        incoming
            .await
            .expect("server handshake with trusted client")
    })
    .await
    .expect("handshake completed within timeout");
    let client_conn = client_task.await.expect("client task");

    // Derive the identity the connector would attach to each handoff.
    let identity = anchors
        .identity_for(&server_conn)
        .expect("trusted client derives an identity");
    assert_eq!(identity.tenant, "acme");

    // The identity must survive its handoff metadata encode/decode.
    let decoded = selium_abi::client_identity::ClientIdentity::decode(&identity.encode())
        .expect("decode identity metadata");
    assert_eq!(decoded, identity);

    drop(client_conn);
    drop(endpoint);
    drop(client_endpoint);
}

/// Unknown SNI is refused: the connector closes the connection before ever
/// contacting an app guest (no discovery context = nothing to contact).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unknown_sni_is_refused_without_guest_contact() {
    let (server_config, cert) = server_config();

    let server_socket = tokio::net::UdpSocket::bind("127.0.0.1:0")
        .await
        .expect("bind server socket");
    let server_addr = server_socket.local_addr().expect("server addr");
    let server_socket = TokioUdpSocket {
        inner: Arc::new(server_socket),
        buf: Mutex::new(vec![0u8; 65536]),
    };
    let endpoint = build_endpoint(
        Arc::new(server_socket),
        Arc::new(TokioRuntime),
        Some(server_config),
    )
    .expect("build server endpoint");

    let client_socket = tokio::net::UdpSocket::bind("127.0.0.1:0")
        .await
        .expect("bind client socket");
    let client_socket = TokioUdpSocket {
        inner: Arc::new(client_socket),
        buf: Mutex::new(vec![0u8; 65536]),
    };
    let mut client_endpoint = build_endpoint(Arc::new(client_socket), Arc::new(TokioRuntime), None)
        .expect("build client endpoint");
    let mut roots = quinn::rustls::RootCertStore::empty();
    roots.add(cert).expect("add root cert");
    client_endpoint.set_default_client_config(
        ClientConfig::with_root_certificates(Arc::new(roots)).expect("client config"),
    );

    // Connect with a valid server name (so TLS succeeds) and drive both sides.
    let client_task = {
        let ep = client_endpoint.clone();
        tokio::spawn(async move {
            ep.connect(server_addr, "localhost")
                .expect("connect")
                .await
                .expect("client connection")
        })
    };
    let server_conn = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let incoming = endpoint.accept().await.expect("server incoming");
        incoming.await.expect("server handshake")
    })
    .await
    .expect("handshake completes");
    let client_conn = client_task.await.expect("client task");

    // The presented SNI is recovered from the handshake.
    assert_eq!(sni_of(&server_conn).as_deref(), Some("localhost"));

    // An empty resolver = no registered route: the connector must refuse and
    // close the connection without contacting any app guest.
    let resolver: selium_connector_quic::resolve::ResolverHandle =
        Arc::new(tokio::sync::Mutex::new(RouteResolver::empty()));
    let anchors = test_anchor_set();
    let limiter = selium_connector_quic::rate::AdmissionLimiter::new(100, 100);
    handle_connection(server_conn.clone(), resolver, Some(anchors), limiter).await;

    let closed = tokio::time::timeout(std::time::Duration::from_secs(5), server_conn.closed())
        .await
        .is_ok();
    assert!(closed, "unknown SNI must be refused and closed");

    drop(client_conn);
    drop(endpoint);
    drop(client_endpoint);
}
