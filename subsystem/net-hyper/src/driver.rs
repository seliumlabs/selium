//! Driver wiring for Hyper-backed HTTP connections.
//! Uses HTTP/1.1 for `NetProtocol::Http` and HTTP/2 for `NetProtocol::Https`.

use std::{
    collections::{HashMap, VecDeque},
    fmt,
    future::Future,
    net::SocketAddr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

use futures_util::future::BoxFuture;
use hyper::{
    Body,
    client::conn::{http1, http2},
    http::{
        header::{InvalidHeaderName, InvalidHeaderValue},
        method::InvalidMethod,
        uri::InvalidUri,
    },
};
use rustls::{ClientConfig, ServerConfig, sign};
use selium_abi::{IoFrame, NetProtocol};
use selium_kernel::{
    drivers::{
        io::IoCapability,
        net::{NetCapability, TlsClientConfig, TlsServerConfig},
    },
    guest_data::GuestError,
};
use thiserror::Error;
use tokio::{
    io::{AsyncRead, AsyncWrite},
    net::TcpListener as TokioTcpListener,
    sync::{Notify, mpsc, oneshot},
};
use tracing::debug;

use crate::{
    client::{connect_stream, read_outbound, write_outbound},
    server::{read_inbound, run_listener, write_inbound},
    tls::{
        build_client_config, build_client_verifier, build_server_config, certified_key_from_config,
        resolve_alpn,
    },
};

pub(crate) type HyperStream = Box<dyn HyperIo + 'static>;

const PENDING_QUEUE: usize = 64;

pub(crate) trait HyperIo: AsyncRead + AsyncWrite + Unpin + Send {}

impl<T> HyperIo for T where T: AsyncRead + AsyncWrite + Unpin + Send {}

pub(crate) enum OutboundSender {
    Http1(http1::SendRequest<Body>),
    Http2(http2::SendRequest<Body>),
}

/// Errors produced by the Hyper-backed listener driver.
#[derive(Error, Debug)]
pub enum HyperError {
    #[error("listener closed before any request arrived")]
    ListenerClosed,
    #[error("port out of range")]
    PortRange,
    #[error("failed to bind listener: {0}")]
    Bind(#[source] std::io::Error),
    #[error("failed to mark listener non-blocking: {0}")]
    NonBlocking(#[source] std::io::Error),
    #[error("failed to connect: {0}")]
    Connect(#[source] std::io::Error),
    #[error("TLS handshake failed: {0}")]
    Tls(#[source] std::io::Error),
    #[error("failed to build TLS config: {0}")]
    Rustls(#[source] rustls::Error),
    #[error("failed to parse certificate chain: {0}")]
    Certificate(String),
    #[error("failed to parse private key: {0}")]
    PrivateKey(String),
    #[error("client certificate provided without private key")]
    ClientKeyMissing,
    #[error("client authentication requires a CA bundle")]
    ClientAuthMissing,
    #[error("failed to configure client authentication: {0}")]
    ClientAuth(String),
    #[error("HTTP connection failed: {0}")]
    Hyper(#[source] hyper::Error),
    #[error("failed to build HTTP message: {0}")]
    Http(#[source] hyper::http::Error),
    #[error("HTTP parse error: {0}")]
    HttpParse(String),
    #[error("HTTP message incomplete")]
    HttpIncomplete,
    #[error("invalid header name: {0}")]
    InvalidHeaderName(#[source] InvalidHeaderName),
    #[error("invalid header value: {0}")]
    InvalidHeaderValue(#[source] InvalidHeaderValue),
    #[error("invalid method: {0}")]
    InvalidMethod(#[source] InvalidMethod),
    #[error("invalid URI: {0}")]
    InvalidUri(#[source] InvalidUri),
    #[error("invalid status code")]
    InvalidStatus,
    #[error("unsupported transfer encoding")]
    TransferEncoding,
    #[error("content length mismatch (expected {expected}, got {actual})")]
    ContentLengthMismatch { expected: usize, actual: usize },
    #[error("host header does not match target domain")]
    HostMismatch,
    #[error("response channel closed")]
    ResponseChannelClosed,
    #[error("mutex poisoned")]
    Lock,
    #[error("operation unsupported")]
    Unsupported,
    #[error("TLS configuration does not match existing listener")]
    TlsConfigMismatch,
    #[error("unsupported protocol: {protocol:?}")]
    UnsupportedProtocol { protocol: NetProtocol },
}

#[derive(Clone, Debug)]
struct TokioExec;

struct ListenerRegistry {
    listeners: Mutex<HashMap<u16, Arc<Listener>>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ListenerTlsProfile {
    cert_chain: Vec<Vec<u8>>,
    alpn: Vec<Vec<u8>>,
    client_ca_pem: Option<Vec<u8>>,
    require_client_auth: bool,
}

struct Listener {
    protocol: NetProtocol,
    domain: String,
    pending_rx: tokio::sync::Mutex<mpsc::Receiver<PendingRequest>>,
    tls_profile: ListenerTlsProfile,
}

pub(crate) struct PendingRequest {
    pub(crate) request_bytes: Vec<u8>,
    pub(crate) responder: oneshot::Sender<Vec<u8>>,
    pub(crate) remote_addr: String,
}

pub(crate) struct OutboundState {
    pub(crate) protocol: NetProtocol,
    pub(crate) domain: String,
    pub(crate) port: u16,
    pub(crate) sender: tokio::sync::Mutex<OutboundSender>,
    pub(crate) response: tokio::sync::Mutex<VecDeque<u8>>,
    pub(crate) response_notify: Notify,
    pub(crate) closed: AtomicBool,
}

pub(crate) struct InboundState {
    pub(crate) request: Mutex<VecDeque<u8>>,
    pub(crate) response: Mutex<Vec<u8>>,
    pub(crate) responder: Mutex<Option<oneshot::Sender<Vec<u8>>>>,
}

pub(crate) enum ConnectionKind {
    Outbound(Arc<OutboundState>),
    Inbound(Arc<InboundState>),
}

/// Handle for a bound HTTP listener.
#[derive(Clone)]
pub struct ListenerHandle {
    listener: Arc<Listener>,
}

/// Hyper-backed network driver.
pub struct HyperDriver {
    registry: Arc<ListenerRegistry>,
    default_cert_chain: Vec<Vec<u8>>,
    default_server_config: Arc<ServerConfig>,
    default_client_config: Arc<ClientConfig>,
}

/// Reader side of an HTTP connection.
pub struct HttpReader {
    state: ConnectionKind,
}

/// Writer side of an HTTP connection.
pub struct HttpWriter {
    state: ConnectionKind,
}

impl fmt::Debug for ListenerHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ListenerHandle")
            .field("protocol", &self.listener.protocol)
            .field("domain", &self.listener.domain)
            .finish()
    }
}

impl ListenerRegistry {
    fn new() -> Self {
        Self {
            listeners: Mutex::new(HashMap::new()),
        }
    }

    fn get_or_try_init(
        &self,
        protocol: NetProtocol,
        domain: &str,
        port: u16,
        tls_profile: ListenerTlsProfile,
        server_config: Arc<ServerConfig>,
    ) -> Result<Arc<Listener>, HyperError> {
        let mut guard = self.listeners.lock().map_err(|_| HyperError::Lock)?;
        if let Some(listener) = guard.get(&port) {
            if listener.protocol != protocol {
                return Err(HyperError::UnsupportedProtocol { protocol });
            }
            if !listener.domain.eq_ignore_ascii_case(domain) {
                return Err(HyperError::HostMismatch);
            }
            if listener.tls_profile != tls_profile {
                return Err(HyperError::TlsConfigMismatch);
            }
            return Ok(Arc::clone(listener));
        }

        let listener = Arc::new(Listener::new(
            protocol,
            domain.to_string(),
            port,
            tls_profile,
            server_config,
        )?);
        guard.insert(port, Arc::clone(&listener));
        Ok(listener)
    }
}

impl Listener {
    fn new(
        protocol: NetProtocol,
        domain: String,
        port: u16,
        tls_profile: ListenerTlsProfile,
        server_config: Arc<ServerConfig>,
    ) -> Result<Self, HyperError> {
        ensure_http_protocol(protocol)?;
        let addr = SocketAddr::from(([0, 0, 0, 0], port));
        let std_listener = std::net::TcpListener::bind(addr).map_err(HyperError::Bind)?;
        std_listener
            .set_nonblocking(true)
            .map_err(HyperError::NonBlocking)?;
        let listener = TokioTcpListener::from_std(std_listener).map_err(HyperError::Bind)?;

        let (pending_tx, pending_rx) = mpsc::channel(PENDING_QUEUE);
        tokio::spawn(run_listener(
            listener,
            protocol,
            domain.clone(),
            server_config,
            pending_tx,
        ));

        Ok(Self {
            protocol,
            domain,
            pending_rx: tokio::sync::Mutex::new(pending_rx),
            tls_profile,
        })
    }
}

impl ListenerHandle {
    fn new(listener: Arc<Listener>) -> Self {
        Self { listener }
    }

    /// Return the domain bound by the listener.
    pub fn domain(&self) -> &str {
        &self.listener.domain
    }

    /// Return the protocol bound by the listener.
    pub fn protocol(&self) -> NetProtocol {
        self.listener.protocol
    }
}

impl HyperDriver {
    /// Create a new driver instance with an already validated certificate and private key.
    pub fn new(certified_key: Arc<sign::CertifiedKey>) -> Result<Arc<Self>, HyperError> {
        let default_cert_chain = certified_key
            .cert
            .iter()
            .map(|cert| cert.as_ref().to_vec())
            .collect::<Vec<_>>();
        let client_verifier = build_client_verifier(None, false)?;
        let default_server_config = build_server_config(
            Arc::clone(&certified_key),
            resolve_alpn(NetProtocol::Https, None),
            client_verifier,
        )?;
        let default_client_config = build_client_config(NetProtocol::Https, None)?;
        Ok(Arc::new(Self {
            registry: Arc::new(ListenerRegistry::new()),
            default_cert_chain,
            default_server_config,
            default_client_config,
        }))
    }
}

impl HttpReader {
    fn outbound(state: Arc<OutboundState>) -> Self {
        Self {
            state: ConnectionKind::Outbound(state),
        }
    }

    fn inbound(state: Arc<InboundState>) -> Self {
        Self {
            state: ConnectionKind::Inbound(state),
        }
    }
}

impl HttpWriter {
    fn outbound(state: Arc<OutboundState>) -> Self {
        Self {
            state: ConnectionKind::Outbound(state),
        }
    }

    fn inbound(state: Arc<InboundState>) -> Self {
        Self {
            state: ConnectionKind::Inbound(state),
        }
    }
}

impl Drop for HttpWriter {
    fn drop(&mut self) {
        match &self.state {
            ConnectionKind::Outbound(state) => {
                state.closed.store(true, Ordering::SeqCst);
                state.response_notify.notify_waiters();
            }
            ConnectionKind::Inbound(state) => {
                let response = match state.response.lock() {
                    Ok(mut guard) => std::mem::take(&mut *guard),
                    Err(err) => {
                        debug!(err = %err, "response buffer lock poisoned");
                        Vec::new()
                    }
                };
                let responder = match state.responder.lock() {
                    Ok(mut guard) => guard.take(),
                    Err(err) => {
                        debug!(err = %err, "response channel lock poisoned");
                        None
                    }
                };
                if let Some(responder) = responder {
                    if responder.send(response).is_err() {
                        debug!("response receiver dropped before completion");
                    }
                }
            }
        }
    }
}

impl NetCapability for HyperDriver {
    type Handle = ListenerHandle;
    type Reader = HttpReader;
    type Writer = HttpWriter;
    type Error = HyperError;

    fn create(
        &self,
        protocol: NetProtocol,
        domain: &str,
        port: u16,
        tls: Option<Arc<TlsServerConfig>>,
    ) -> BoxFuture<'_, Result<Self::Handle, Self::Error>> {
        let registry = Arc::clone(&self.registry);
        let domain = domain.to_string();
        let default_cert_chain = self.default_cert_chain.clone();
        let default_server_config = Arc::clone(&self.default_server_config);

        Box::pin(async move {
            ensure_http_protocol(protocol)?;
            let (server_config, tls_profile) = match tls.as_ref() {
                Some(config) => {
                    let alpn = resolve_alpn(protocol, config.alpn.as_ref());
                    let client_verifier = build_client_verifier(
                        config.client_ca_pem.as_ref(),
                        config.require_client_auth,
                    )?;
                    let (certified_key, cert_chain) = certified_key_from_config(config)?;
                    let server_config =
                        build_server_config(certified_key, alpn.clone(), client_verifier)?;
                    let profile = ListenerTlsProfile {
                        cert_chain,
                        alpn,
                        client_ca_pem: config.client_ca_pem.clone(),
                        require_client_auth: config.require_client_auth,
                    };
                    (server_config, profile)
                }
                None => {
                    let alpn = resolve_alpn(protocol, None);
                    let profile = ListenerTlsProfile {
                        cert_chain: default_cert_chain,
                        alpn,
                        client_ca_pem: None,
                        require_client_auth: false,
                    };
                    (default_server_config, profile)
                }
            };
            let listener =
                registry.get_or_try_init(protocol, &domain, port, tls_profile, server_config)?;
            Ok(ListenerHandle::new(listener))
        })
    }

    fn connect(
        &self,
        protocol: NetProtocol,
        domain: &str,
        port: u16,
        tls: Option<Arc<TlsClientConfig>>,
    ) -> BoxFuture<'_, Result<(Self::Reader, Self::Writer, String), Self::Error>> {
        let domain = domain.to_string();
        let default_client_config = Arc::clone(&self.default_client_config);

        Box::pin(async move {
            ensure_http_protocol(protocol)?;
            let tls = tls.as_deref();
            let client_config = match tls {
                Some(config) => build_client_config(protocol, Some(config))?,
                None => default_client_config,
            };
            let stream = connect_stream(protocol, &domain, port, client_config).await?;
            let sender = match protocol {
                NetProtocol::Http => {
                    let (sender, connection) =
                        http1::handshake(stream).await.map_err(HyperError::Hyper)?;
                    tokio::spawn(async move {
                        if let Err(err) = connection.await {
                            debug!(err = %err, "http connection terminated");
                        }
                    });
                    OutboundSender::Http1(sender)
                }
                NetProtocol::Https => {
                    let (sender, connection) = http2::handshake(TokioExec, stream)
                        .await
                        .map_err(HyperError::Hyper)?;
                    tokio::spawn(async move {
                        if let Err(err) = connection.await {
                            debug!(err = %err, "http connection terminated");
                        }
                    });
                    OutboundSender::Http2(sender)
                }
                _ => return Err(HyperError::UnsupportedProtocol { protocol }),
            };

            let state = Arc::new(OutboundState {
                protocol,
                domain: domain.clone(),
                port,
                sender: tokio::sync::Mutex::new(sender),
                response: tokio::sync::Mutex::new(VecDeque::new()),
                response_notify: Notify::new(),
                closed: AtomicBool::new(false),
            });

            let reader = HttpReader::outbound(Arc::clone(&state));
            let writer = HttpWriter::outbound(state);
            Ok((reader, writer, format!("{domain}:{port}")))
        })
    }

    fn accept(
        &self,
        handle: &Self::Handle,
    ) -> BoxFuture<'_, Result<(Self::Reader, Self::Writer, String), Self::Error>> {
        let listener = Arc::clone(&handle.listener);

        Box::pin(async move {
            let pending = {
                let mut guard = listener.pending_rx.lock().await;
                guard.recv().await
            }
            .ok_or(HyperError::ListenerClosed)?;

            let state = Arc::new(InboundState {
                request: Mutex::new(pending.request_bytes.into()),
                response: Mutex::new(Vec::new()),
                responder: Mutex::new(Some(pending.responder)),
            });

            let reader = HttpReader::inbound(Arc::clone(&state));
            let writer = HttpWriter::inbound(state);
            Ok((reader, writer, pending.remote_addr))
        })
    }
}

impl IoCapability for HyperDriver {
    type Handle = ();
    type Reader = HttpReader;
    type Writer = HttpWriter;
    type Error = HyperError;

    fn new_writer(&self, _handle: &Self::Handle) -> Result<Self::Writer, Self::Error> {
        Err(HyperError::Unsupported)
    }

    fn new_reader(&self, _handle: &Self::Handle) -> Result<Self::Reader, Self::Error> {
        Err(HyperError::Unsupported)
    }

    async fn read(&self, reader: &mut Self::Reader, len: usize) -> Result<IoFrame, Self::Error> {
        match &reader.state {
            ConnectionKind::Outbound(state) => read_outbound(state, len).await,
            ConnectionKind::Inbound(state) => read_inbound(state, len),
        }
    }

    async fn write(&self, writer: &mut Self::Writer, bytes: &[u8]) -> Result<(), Self::Error> {
        match &writer.state {
            ConnectionKind::Outbound(state) => write_outbound(state, bytes).await,
            ConnectionKind::Inbound(state) => write_inbound(state, bytes),
        }
    }
}

impl From<HyperError> for GuestError {
    fn from(value: HyperError) -> Self {
        match value {
            HyperError::HttpParse(_) => GuestError::InvalidArgument,
            HyperError::HttpIncomplete => GuestError::InvalidArgument,
            HyperError::Certificate(_) => GuestError::InvalidArgument,
            HyperError::PrivateKey(_) => GuestError::InvalidArgument,
            HyperError::ClientKeyMissing => GuestError::InvalidArgument,
            HyperError::ClientAuthMissing => GuestError::InvalidArgument,
            HyperError::ClientAuth(_) => GuestError::InvalidArgument,
            HyperError::InvalidHeaderName(_) => GuestError::InvalidArgument,
            HyperError::InvalidHeaderValue(_) => GuestError::InvalidArgument,
            HyperError::InvalidMethod(_) => GuestError::InvalidArgument,
            HyperError::InvalidUri(_) => GuestError::InvalidArgument,
            HyperError::InvalidStatus => GuestError::InvalidArgument,
            HyperError::ContentLengthMismatch { .. } => GuestError::InvalidArgument,
            HyperError::HostMismatch => GuestError::InvalidArgument,
            HyperError::TlsConfigMismatch => GuestError::InvalidArgument,
            HyperError::UnsupportedProtocol { .. } => GuestError::InvalidArgument,
            HyperError::TransferEncoding => GuestError::InvalidArgument,
            _ => GuestError::Subsystem(value.to_string()),
        }
    }
}

impl<F> hyper::rt::Executor<F> for TokioExec
where
    F: Future<Output = ()> + Send + 'static,
{
    fn execute(&self, fut: F) {
        tokio::spawn(fut);
    }
}

fn ensure_http_protocol(protocol: NetProtocol) -> Result<(), HyperError> {
    match protocol {
        NetProtocol::Http | NetProtocol::Https => Ok(()),
        _ => Err(HyperError::UnsupportedProtocol { protocol }),
    }
}
