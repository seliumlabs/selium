//! External client library for interacting with Selium runtimes and their guests.
//!
//! You **should** use this library in code that _does not_ run on a Selium runtime.
//! You **should not** use this library in code that _does_ run on a Selium runtime.
//!
//! This library can be used for:
//!  1. Orchestration - creating channels, managing processes, etc.
//!  2. Data I/O - publishing payloads, calling RPC servers, etc.
//!
//! # Examples
//! ```no_run
//! use futures::{SinkExt, StreamExt, pin_mut};
//! use selium_client::{Channel, ClientConfigBuilder, ClientError};
//!
//! #[tokio::main]
//! async fn main() -> Result<(), ClientError> {
//!     let client = ClientConfigBuilder::default().connect().await?;
//!     let chunk_size = 64 * 1024;
//!     let channel = Channel::create(&client, chunk_size).await?;
//!
//!     let mut publisher = channel.publish().await?;
//!     publisher.send(b"ping".to_vec()).await?;
//!
//!     let mut subscriber = channel.subscribe(chunk_size).await?;
//!     pin_mut!(subscriber);
//!     if let Some(frame) = subscriber.next().await {
//!         let payload = frame?;
//!         eprintln!("received {} bytes", payload.len());
//!     }
//!
//!     Ok(())
//! }
//! ```
#![deny(missing_docs)]
use std::{
    collections::VecDeque,
    fmt::{Debug, Display},
    fs,
    io::ErrorKind,
    net::SocketAddr,
    path::{Path, PathBuf},
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};

use futures::{Sink, Stream};
use quinn::{
    ClientConfig, ConnectError, Connection, ConnectionError, Endpoint, RecvStream, SendStream,
    TransportConfig, WriteError, crypto::rustls::QuicClientConfig, rustls,
};
use rkyv::{Archive, Deserialize, Serialize};
use rustls::{
    RootCertStore,
    pki_types::{CertificateDer, PrivateKeyDer},
};
use rustls_pki_types::{PrivatePkcs1KeyDer, PrivatePkcs8KeyDer, pem::SliceIter};
use selium_abi::{
    AbiScalarValue, AbiSignature, Capability, EntrypointArg, EntrypointInvocation, GuestResourceId,
    GuestUint, IoFrame, RkyvEncode, decode_rkyv, encode_rkyv,
};
use thiserror::Error;
use tokio::net::lookup_host;
use tracing::debug;

type Result<T> = std::result::Result<T, ClientError>;

/// Default domain exposed by the remote-client guest.
pub const DEFAULT_DOMAIN: &str = "localhost";
/// Default port exposed by the remote-client guest.
pub const DEFAULT_PORT: u16 = 7000;
/// Maximum size allowed for RPC server replies.
pub const DEFAULT_RESPONSE_LIMIT: usize = 8 * 1024;

/// Errors returned by the Selium client.
#[derive(Debug, Error)]
pub enum ClientError {
    /// X.509 material could not be parsed.
    #[error("failed to parse certificate: {0}")]
    Certificate(String),
    /// TLS configuration could not be constructed.
    #[error("failed to build TLS config: {0}")]
    Tls(#[source] rustls::Error),
    /// DNS resolution failed for the given target.
    #[error("failed to resolve {target}: {source}")]
    Resolve {
        /// The `domain:port` string that was resolved.
        target: String,
        /// The underlying resolution error.
        #[source]
        source: std::io::Error,
    },
    /// Failed to bind a local QUIC endpoint.
    #[error("failed to open client endpoint: {0}")]
    Endpoint(#[source] std::io::Error),
    /// The QUIC connection attempt failed.
    #[error("failed to connect: {0}")]
    Connect(#[source] ConnectError),
    /// An established QUIC connection returned an error.
    #[error("connection failed: {0}")]
    Connection(#[source] ConnectionError),
    /// A write to the QUIC stream failed.
    #[error("stream write failed: {0}")]
    Write(#[source] WriteError),
    /// The QUIC send stream could not be finished cleanly.
    #[error("stream finish failed: {0}")]
    Finish(String),
    /// A read from the QUIC stream failed.
    #[error("stream read failed: {0}")]
    Read(String),
    /// Failed to encode a request payload.
    #[error("encode request: {0}")]
    Encode(String),
    /// Failed to decode a response payload.
    #[error("decode response: {0}")]
    Decode(String),
    /// Input arguments were rejected locally (before contacting the remote).
    #[error("invalid request: {0}")]
    InvalidArgument(&'static str),
    /// The remote `remote-client` guest returned an error string.
    #[error("remote error: {0}")]
    Remote(String),
    /// The remote `remote-client` guest sent an unexpected response.
    #[error("unexpected response from remote client")]
    UnexpectedResponse,
}

/// Configures a [`Client`] connection to the runtime.
#[derive(Clone, Debug)]
pub struct ClientConfigBuilder {
    domain: String,
    port: u16,
    response_limit: usize,
    cert_dir: PathBuf,
}

/// Connection to a Selium runtime.
#[derive(Clone, Debug)]
pub struct Client {
    inner: Arc<ClientInner>,
}

#[derive(Debug)]
struct ClientInner {
    endpoint: Endpoint,
    server_addr: SocketAddr,
    server_name: String,
    response_limit: usize,
}

/// Data pipelines that transmit bytes bidirectionally.
#[derive(Clone, Debug)]
pub struct Channel {
    client: Client,
    handle: GuestResourceId,
}

/// Handle to a running process in the runtime.
#[derive(Clone, Debug)]
pub struct Process {
    client: Client,
    handle: GuestResourceId,
}

/// Configures a process to be launched in the runtime.
#[derive(Clone, Debug, PartialEq)]
pub struct ProcessBuilder {
    module_id: String,
    entrypoint: String,
    capabilities: Vec<Capability>,
    signature: AbiSignature,
    args: Vec<EntrypointArg>,
}

#[derive(Debug, Archive, Serialize, Deserialize)]
#[rkyv(bytecheck())]
enum Request {
    ChannelCreate(GuestUint),
    ChannelDelete(GuestResourceId),
    Subscribe(ChannelRef, GuestUint),
    Publish(GuestResourceId),
    ProcessStart(ProcessStartRequest),
    ProcessStop(GuestResourceId),
    ProcessLogChannel(GuestResourceId),
}

#[derive(Debug, Archive, Serialize, Deserialize)]
#[rkyv(bytecheck())]
enum Response {
    ChannelCreate(GuestResourceId),
    ChannelRead(IoFrame),
    ChannelWrite(GuestUint),
    ProcessStart(GuestResourceId),
    ProcessLogChannel(GuestResourceId),
    Ok,
    Error(String),
}

#[derive(Debug, Clone, PartialEq, Archive, Serialize, Deserialize)]
#[rkyv(bytecheck())]
enum ChannelRef {
    Strong(GuestResourceId),
    Shared(GuestResourceId),
}

#[derive(Debug, Clone, PartialEq, Archive, Serialize, Deserialize)]
#[rkyv(bytecheck())]
struct ProcessStartRequest {
    module_id: String,
    entrypoint: String,
    capabilities: Vec<Capability>,
    signature: AbiSignature,
    args: Vec<EntrypointArg>,
}

struct QuicSession {
    connection: Connection,
    send: SendStream,
    recv: RecvStream,
}

enum PublishState {
    Ready(SendStream),
    Writing(Pin<Box<dyn std::future::Future<Output = Result<SendStream>> + Send>>),
    Closed,
}

/// Sink that drains payloads into the channel.
pub struct Publisher {
    _connection: Connection,
    state: PublishState,
}

/// Stream of payloads read from a channel.
pub struct Subscriber {
    inner: Pin<Box<dyn Stream<Item = Result<Vec<u8>>> + Send>>,
}

impl Default for ClientConfigBuilder {
    /// Create a builder using the default Selium client settings.
    fn default() -> Self {
        Self {
            domain: DEFAULT_DOMAIN.to_string(),
            port: DEFAULT_PORT,
            response_limit: DEFAULT_RESPONSE_LIMIT,
            cert_dir: default_cert_dir(),
        }
    }
}

impl ClientConfigBuilder {
    /// Override the target domain name.
    pub fn domain(mut self, domain: impl Into<String>) -> Self {
        self.domain = domain.into();
        self
    }

    /// Override the target port.
    pub fn port(mut self, port: u16) -> Self {
        self.port = port;
        self
    }

    /// Override the maximum response size accepted from the remote client.
    pub fn response_limit(mut self, limit: usize) -> Self {
        self.response_limit = limit.max(1);
        self
    }

    /// Override the directory containing the client and CA certificates.
    pub fn certificate_directory(mut self, dir: impl Into<PathBuf>) -> Self {
        self.cert_dir = dir.into();
        self
    }

    /// Build a [`Client`] with the provided settings.
    ///
    /// # Examples
    /// ```no_run
    /// # async fn example() -> Result<(), selium_client::ClientError> {
    /// let _client = selium_client::ClientConfigBuilder::default()
    ///     .domain("api.selium.io")
    ///     .port(7000)
    ///     .connect()
    ///     .await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn connect(self) -> Result<Client> {
        Client::connect_with(self).await
    }
}

impl Client {
    /// Construct a client using the defaults from [`ClientConfigBuilder`].
    pub async fn connect() -> Result<Self> {
        Client::connect_with(ClientConfigBuilder::default()).await
    }

    async fn connect_with(config: ClientConfigBuilder) -> Result<Self> {
        let server_addr = resolve_socket(&config.domain, config.port).await?;
        let bind_addr = match server_addr {
            SocketAddr::V4(_) => SocketAddr::from(([0, 0, 0, 0], 0)),
            SocketAddr::V6(_) => SocketAddr::from(([0, 0, 0, 0, 0, 0, 0, 0], 0)),
        };

        let mut endpoint = Endpoint::client(bind_addr).map_err(ClientError::Endpoint)?;
        endpoint.set_default_client_config(build_client_config(&config.cert_dir)?);

        Ok(Self {
            inner: Arc::new(ClientInner {
                endpoint,
                server_addr,
                server_name: config.domain,
                response_limit: config.response_limit,
            }),
        })
    }

    async fn open_session(&self) -> Result<QuicSession> {
        let connecting = self
            .inner
            .endpoint
            .connect(self.inner.server_addr, &self.inner.server_name)
            .map_err(ClientError::Connect)?;
        let connection = connecting.await.map_err(ClientError::Connection)?;
        let (send, recv) = connection
            .open_bi()
            .await
            .map_err(ClientError::Connection)?;
        Ok(QuicSession {
            connection,
            send,
            recv,
        })
    }

    async fn request(&self, request: Request) -> Result<Response> {
        let mut session = self.open_session().await?;
        let payload = encode_rkyv(&request).map_err(|err| ClientError::Encode(err.to_string()))?;
        debug!("sending request of {} bytes", payload.len());
        session
            .send
            .write_all(&payload)
            .await
            .map_err(ClientError::Write)?;
        session
            .send
            .finish()
            .map_err(|_| ClientError::Finish("stream closed".to_string()))?;
        debug!("request sent, awaiting response");
        let mut buffer = VecDeque::new();
        let response =
            read_response(&mut session.recv, self.inner.response_limit, &mut buffer).await?;
        match response {
            Response::Error(message) => Err(ClientError::Remote(message)),
            other => Ok(other),
        }
    }
}

impl Channel {
    /// Create a channel using the remote-client guest.
    pub async fn create(client: &Client, capacity: u32) -> Result<Self> {
        let response = client.request(Request::ChannelCreate(capacity)).await?;
        match response {
            Response::ChannelCreate(handle) => Ok(Self::new(client, handle)),
            Response::Error(msg) => Err(ClientError::Remote(msg)),
            _ => Err(ClientError::UnexpectedResponse),
        }
    }

    /// Construct a handle to an existing channel.
    ///
    /// # Safety
    /// `handle` must be a valid channel capability minted for the current client; forged or stale
    /// handles will be rejected by the remote guest and may cause the connection to be closed.
    pub fn new(client: &Client, handle: GuestResourceId) -> Self {
        Self {
            client: client.clone(),
            handle,
        }
    }

    /// Delete this channel.
    pub async fn delete(self) -> Result<()> {
        let response = self
            .client
            .request(Request::ChannelDelete(self.handle))
            .await?;
        match response {
            Response::Ok => Ok(()),
            Response::Error(msg) => Err(ClientError::Remote(msg)),
            _ => Err(ClientError::UnexpectedResponse),
        }
    }

    /// Subscribe to this channel as a [`Subscriber`] of byte payloads.
    ///
    /// # Examples
    /// ```no_run
    /// use futures::{StreamExt, pin_mut};
    ///
    /// # async fn example(channel: &selium_client::Channel) -> Result<(), selium_client::ClientError> {
    /// let mut subscriber = channel.subscribe(64 * 1024).await?;
    /// pin_mut!(subscriber);
    /// if let Some(frame) = subscriber.next().await {
    ///     let payload = frame?;
    ///     eprintln!("received {} bytes", payload.len());
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub async fn subscribe(&self, chunk_size: u32) -> Result<Subscriber> {
        self.subscribe_inner(ChannelRef::Strong(self.handle), chunk_size)
            .await
    }

    /// Subscribe to this channel as a [`Subscriber`] of byte payloads.
    ///
    /// # Examples
    /// ```no_run
    /// use futures::{StreamExt, pin_mut};
    ///
    /// # async fn example(channel: &selium_client::Channel) -> Result<(), selium_client::ClientError> {
    /// let mut subscriber = channel.subscribe_shared(64 * 1024).await?;
    /// pin_mut!(subscriber);
    /// if let Some(frame) = subscriber.next().await {
    ///     let payload = frame?;
    ///     eprintln!("received {} bytes", payload.len());
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub async fn subscribe_shared(&self, chunk_size: u32) -> Result<Subscriber> {
        self.subscribe_inner(ChannelRef::Shared(self.handle), chunk_size)
            .await
    }

    async fn subscribe_inner(&self, target: ChannelRef, chunk_size: u32) -> Result<Subscriber> {
        let mut session = self.client.open_session().await?;
        let chunk_size = GuestUint::try_from(chunk_size)
            .map_err(|_| ClientError::InvalidArgument("chunk size exceeds u32::MAX"))?;
        let payload = encode_rkyv(&Request::Subscribe(target, chunk_size))
            .map_err(|err| ClientError::Encode(err.to_string()))?;
        session
            .send
            .write_all(&payload)
            .await
            .map_err(ClientError::Write)?;
        session
            .send
            .finish()
            .map_err(|_| ClientError::Finish("stream closed".to_string()))?;
        let max_frame = usize::try_from(chunk_size)
            .map_err(|_| ClientError::InvalidArgument("chunk size exceeds usize::MAX"))?;
        let connection = session.connection.clone();
        let stream = futures::stream::unfold(
            (session.recv, VecDeque::new(), max_frame, connection),
            move |(mut recv, mut buffer, max_frame, connection)| async move {
                match read_subscribed_frame(&mut recv, &mut buffer, max_frame).await {
                    Ok(Some(frame)) => Some((Ok(frame), (recv, buffer, max_frame, connection))),
                    Ok(None) => None,
                    Err(err) => Some((Err(err), (recv, buffer, max_frame, connection))),
                }
            },
        );
        Ok(Subscriber::new(stream))
    }

    /// Create a publisher for this channel. The returned [`Sink`] streams raw payloads.
    ///
    /// # Examples
    /// ```no_run
    /// use futures::SinkExt;
    ///
    /// # async fn example(channel: &selium_client::Channel) -> Result<(), selium_client::ClientError> {
    /// let mut publisher = channel.publish().await?;
    /// publisher.send(b"hello".to_vec()).await?;
    /// publisher.close().await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn publish(&self) -> Result<Publisher> {
        let mut session = self.client.open_session().await?;
        let payload = encode_rkyv(&Request::Publish(self.handle))
            .map_err(|err| ClientError::Encode(err.to_string()))?;
        session
            .send
            .write_all(&payload)
            .await
            .map_err(ClientError::Write)?;

        let mut buffer = VecDeque::new();
        read_response_once(
            &mut session.recv,
            self.client.inner.response_limit,
            &mut buffer,
        )
        .await?;

        Ok(Publisher {
            _connection: session.connection,
            state: PublishState::Ready(session.send),
        })
    }

    /// Expose the raw handle.
    pub fn handle(&self) -> GuestResourceId {
        self.handle
    }
}

impl ProcessBuilder {
    /// Create a new builder targeting the supplied module and entrypoint.
    pub fn new(module_id: impl Into<String>, entrypoint: impl Into<String>) -> Self {
        Self {
            module_id: module_id.into(),
            entrypoint: entrypoint.into(),
            capabilities: vec![Capability::ChannelLifecycle, Capability::ChannelWriter],
            signature: AbiSignature::new(Vec::new(), Vec::new()),
            args: Vec::new(),
        }
    }

    /// Add a capability the process should receive.
    pub fn capability(mut self, capability: Capability) -> Self {
        if !self.capabilities.contains(&capability) {
            self.capabilities.push(capability);
        }
        self
    }

    /// Specify the entrypoint ABI signature.
    pub fn signature(mut self, signature: AbiSignature) -> Self {
        self.signature = signature;
        self
    }

    /// Append a scalar argument.
    pub fn arg_scalar(mut self, value: AbiScalarValue) -> Self {
        self.args.push(EntrypointArg::Scalar(value));
        self
    }

    /// Append a UTF-8 string argument.
    pub fn arg_utf8(self, value: impl Into<String>) -> Self {
        self.arg_buffer(value.into().into_bytes())
    }

    /// Append an rkyv-encoded argument.
    ///
    /// # Examples
    /// ```no_run
    /// use rkyv::{Archive, Deserialize, Serialize};
    ///
    /// #[derive(Archive, Deserialize, Serialize)]
    /// struct Args {
    ///     value: u32,
    /// }
    ///
    /// # fn example() -> Result<(), selium_client::ClientError> {
    /// let _builder = selium_client::ProcessBuilder::new("module", "entry")
    ///     .arg_rkyv(&Args { value: 42 })?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn arg_rkyv<T: RkyvEncode>(self, value: &T) -> Result<Self> {
        let bytes = encode_rkyv(value).map_err(|err| ClientError::Encode(err.to_string()))?;
        Ok(self.arg_buffer(bytes))
    }

    /// Append a raw buffer argument.
    pub fn arg_buffer(mut self, value: impl Into<Vec<u8>>) -> Self {
        self.args.push(EntrypointArg::Buffer(value.into()));
        self
    }

    /// Append a process handle argument.
    pub fn arg_resource(mut self, handle: impl Into<GuestResourceId>) -> Self {
        self.args.push(EntrypointArg::Resource(handle.into()));
        self
    }

    fn build_request(self) -> Result<ProcessStartRequest> {
        let entrypoint = EntrypointInvocation::new(self.signature.clone(), self.args.clone())
            .map_err(|_| ClientError::InvalidArgument("arguments do not satisfy the signature"))?;

        Ok(ProcessStartRequest {
            module_id: self.module_id,
            entrypoint: self.entrypoint,
            capabilities: self.capabilities,
            signature: entrypoint.signature().clone(),
            args: entrypoint.args,
        })
    }
}

impl Process {
    /// Construct a handle to an existing process.
    ///
    /// # Safety
    /// `handle` must be a valid process capability minted for the current client; forged or stale
    /// handles will be rejected by the remote guest and may cause the connection to be closed.
    pub fn new(client: &Client, handle: GuestResourceId) -> Self {
        Self {
            client: client.clone(),
            handle,
        }
    }

    /// Start a process using the provided builder.
    pub async fn start(client: &Client, builder: ProcessBuilder) -> Result<Self> {
        let request = builder.build_request()?;
        let response = client.request(Request::ProcessStart(request)).await?;
        match response {
            Response::ProcessStart(handle) => Ok(Self::new(client, handle)),
            Response::Error(msg) => Err(ClientError::Remote(msg)),
            _ => Err(ClientError::UnexpectedResponse),
        }
    }

    /// Access the raw process handle.
    pub fn handle(&self) -> GuestResourceId {
        self.handle
    }

    /// Fetch the shared logging channel handle for this process.
    pub async fn log_channel(&self) -> Result<Channel> {
        let response = self
            .client
            .request(Request::ProcessLogChannel(self.handle))
            .await?;
        match response {
            Response::ProcessLogChannel(handle) => Ok(Channel::new(&self.client, handle)),
            Response::Error(msg) => Err(ClientError::Remote(msg)),
            _ => Err(ClientError::UnexpectedResponse),
        }
    }

    /// Stop the referenced process.
    pub async fn stop(self) -> Result<()> {
        let response = self
            .client
            .request(Request::ProcessStop(self.handle))
            .await?;
        match response {
            Response::Ok => Ok(()),
            Response::Error(msg) => Err(ClientError::Remote(msg)),
            _ => Err(ClientError::UnexpectedResponse),
        }
    }
}

impl Display for Process {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Process({})", self.handle)
    }
}

impl Subscriber {
    fn new(stream: impl Stream<Item = Result<Vec<u8>>> + Send + 'static) -> Self {
        Self {
            inner: Box::pin(stream),
        }
    }
}

impl Stream for Subscriber {
    type Item = Result<Vec<u8>>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        this.inner.as_mut().poll_next(cx)
    }
}

impl Sink<Vec<u8>> for Publisher {
    type Error = ClientError;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<()>> {
        let this = self.get_mut();
        match &mut this.state {
            PublishState::Ready(_) => Poll::Ready(Ok(())),
            PublishState::Writing(fut) => match fut.as_mut().poll(cx) {
                Poll::Ready(Ok(stream)) => {
                    this.state = PublishState::Ready(stream);
                    Poll::Ready(Ok(()))
                }
                Poll::Ready(Err(err)) => {
                    this.state = PublishState::Closed;
                    Poll::Ready(Err(err))
                }
                Poll::Pending => Poll::Pending,
            },
            PublishState::Closed => Poll::Ready(Err(ClientError::UnexpectedResponse)),
        }
    }

    fn start_send(self: Pin<&mut Self>, item: Vec<u8>) -> Result<()> {
        let this = self.get_mut();
        match std::mem::replace(&mut this.state, PublishState::Closed) {
            PublishState::Ready(stream) => {
                this.state = PublishState::Writing(Box::pin(write_once(stream, item)));
                Ok(())
            }
            other => {
                this.state = other;
                Err(ClientError::InvalidArgument("publisher not ready"))
            }
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<()>> {
        self.poll_ready(cx)
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<()>> {
        let this = self.get_mut();
        loop {
            match std::mem::replace(&mut this.state, PublishState::Closed) {
                PublishState::Ready(mut stream) => {
                    stream
                        .finish()
                        .map_err(|_| ClientError::Finish("stream closed".to_string()))?;
                    this.state = PublishState::Closed;
                    return Poll::Ready(Ok(()));
                }
                PublishState::Writing(mut fut) => match fut.as_mut().poll(cx) {
                    Poll::Ready(Ok(stream)) => {
                        this.state = PublishState::Ready(stream);
                    }
                    Poll::Ready(Err(err)) => {
                        this.state = PublishState::Closed;
                        return Poll::Ready(Err(err));
                    }
                    Poll::Pending => {
                        this.state = PublishState::Writing(fut);
                        return Poll::Pending;
                    }
                },
                PublishState::Closed => return Poll::Ready(Ok(())),
            }
        }
    }
}

async fn read_subscribed_frame(
    recv: &mut RecvStream,
    buffer: &mut VecDeque<u8>,
    max_frame: usize,
) -> Result<Option<Vec<u8>>> {
    loop {
        if let Some(frame) = try_parse_frame(
            buffer,
            max_frame,
            "frame length exceeds subscribed chunk size",
        )? {
            return Ok(Some(frame));
        }

        let mut chunk = vec![0u8; max_frame.max(4)];
        match recv.read(&mut chunk).await {
            Ok(Some(len)) if len > 0 => {
                chunk.truncate(len);
                buffer.extend(chunk);
            }
            Ok(Some(_)) => {}
            Ok(None) => {
                if buffer.is_empty() {
                    return Ok(None);
                }
                return Err(ClientError::Decode(
                    "stream ended with a partial frame".to_string(),
                ));
            }
            Err(err) => return Err(ClientError::Read(err.to_string())),
        }
    }
}

fn try_parse_frame(
    buffer: &mut VecDeque<u8>,
    max_frame: usize,
    limit_error: &'static str,
) -> Result<Option<Vec<u8>>> {
    const LENGTH_PREFIX: usize = 4;
    if buffer.len() < LENGTH_PREFIX {
        return Ok(None);
    }

    let mut len_bytes = [0u8; LENGTH_PREFIX];
    for (dst, src) in len_bytes.iter_mut().zip(buffer.iter().take(LENGTH_PREFIX)) {
        *dst = *src;
    }
    let frame_len = u32::from_le_bytes(len_bytes) as usize;
    if frame_len > max_frame {
        return Err(ClientError::InvalidArgument(limit_error));
    }

    if buffer.len() < LENGTH_PREFIX + frame_len {
        return Ok(None);
    }

    buffer.drain(..LENGTH_PREFIX);
    let payload = buffer.drain(..frame_len).collect();
    Ok(Some(payload))
}

async fn read_response_frame(
    recv: &mut RecvStream,
    limit: usize,
    buffer: &mut VecDeque<u8>,
) -> Result<Vec<u8>> {
    let max_buffer = limit.saturating_add(4);
    loop {
        if let Some(frame) = try_parse_frame(
            buffer,
            limit,
            "response frame length exceeds response limit",
        )? {
            return Ok(frame);
        }

        let mut chunk = vec![0u8; max_buffer.saturating_sub(buffer.len()).max(4)];
        match recv.read(&mut chunk).await {
            Ok(Some(len)) if len > 0 => {
                debug!("read {len} bytes from response stream");
                chunk.truncate(len);
                buffer.extend(chunk);
                if buffer.len() > max_buffer {
                    return Err(ClientError::Decode(
                        "response exceeded maximum size".to_string(),
                    ));
                }
            }
            Ok(Some(_)) => {}
            Ok(None) => {
                return Err(ClientError::Decode(
                    "response stream ended before frame".to_string(),
                ));
            }
            Err(err) => return Err(ClientError::Read(err.to_string())),
        }
    }
}

async fn read_response(
    recv: &mut RecvStream,
    limit: usize,
    buffer: &mut VecDeque<u8>,
) -> Result<Response> {
    let payload = read_response_frame(recv, limit, buffer).await?;
    decode_rkyv::<Response>(&payload).map_err(|err| ClientError::Decode(err.to_string()))
}

async fn read_response_once(
    recv: &mut RecvStream,
    limit: usize,
    buffer: &mut VecDeque<u8>,
) -> Result<()> {
    let response = read_response(recv, limit, buffer).await?;
    match response {
        Response::Ok => Ok(()),
        Response::Error(msg) => Err(ClientError::Remote(msg)),
        _ => Err(ClientError::UnexpectedResponse),
    }
}

async fn resolve_socket(domain: &str, port: u16) -> Result<SocketAddr> {
    let target = format!("{domain}:{port}");
    let addrs: Vec<SocketAddr> = lookup_host(&target)
        .await
        .map_err(|source| ClientError::Resolve {
            target: target.clone(),
            source,
        })?
        .collect();

    addrs
        .iter()
        .copied()
        .find(SocketAddr::is_ipv4)
        .or_else(|| addrs.into_iter().next())
        .ok_or_else(|| ClientError::Resolve {
            target,
            source: std::io::Error::new(
                ErrorKind::AddrNotAvailable,
                "no socket addresses returned",
            ),
        })
}

fn build_client_config(cert_dir: &Path) -> Result<ClientConfig> {
    let provider = rustls::crypto::ring::default_provider();
    let tls_builder = rustls::ClientConfig::builder_with_provider(provider.into())
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(ClientError::Tls)?;
    let roots = build_root_store(cert_dir)?;
    let client_cert = read_certificate(cert_dir, "client.crt")?;
    let client_key = read_certificate(cert_dir, "client.key")?;
    let cert_chain = parse_certificates(&client_cert)?;
    let key = parse_private_key(&client_key)?;
    let tls_config = tls_builder
        .with_root_certificates(roots)
        .with_client_auth_cert(cert_chain, key)
        .map_err(ClientError::Tls)?;

    let crypto = QuicClientConfig::try_from(Arc::new(tls_config))
        .map_err(|_| ClientError::Certificate("failed to select QUIC cipher suite".to_string()))?;
    let mut client_config = ClientConfig::new(Arc::new(crypto));
    let mut transport = TransportConfig::default();
    transport.keep_alive_interval(Some(Duration::from_secs(10)));
    client_config.transport_config(Arc::new(transport));
    Ok(client_config)
}

fn build_root_store(cert_dir: &Path) -> Result<RootCertStore> {
    let mut store = RootCertStore::empty();
    let ca_cert = read_certificate(cert_dir, "ca.crt")?;
    for cert in parse_certificates(&ca_cert)? {
        store
            .add(cert)
            .map_err(|_| ClientError::Certificate("add CA certificate".to_string()))?;
    }
    Ok(store)
}

fn read_certificate(cert_dir: &Path, name: &str) -> Result<Vec<u8>> {
    let path = cert_dir.join(name);
    fs::read(&path)
        .map_err(|err| ClientError::Certificate(format!("read {}: {}", path.display(), err)))
}

fn default_cert_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../certs")
}

fn parse_certificates(bytes: &[u8]) -> Result<Vec<CertificateDer<'static>>> {
    let parsed: std::result::Result<Vec<_>, _> = SliceIter::new(bytes).collect();
    let parsed = parsed.map_err(|err| ClientError::Certificate(err.to_string()))?;
    if !parsed.is_empty() {
        return Ok(parsed);
    }

    Ok(vec![CertificateDer::from(bytes.to_vec())])
}

fn parse_private_key(bytes: &[u8]) -> Result<PrivateKeyDer<'static>> {
    let pkcs8_result: std::result::Result<Vec<PrivatePkcs8KeyDer>, _> =
        SliceIter::new(bytes).collect();
    let pkcs8 = pkcs8_result.map_err(|err| ClientError::Certificate(err.to_string()))?;
    if let Some(key) = pkcs8.into_iter().next() {
        return Ok(key.into());
    }

    let rsa_result: std::result::Result<Vec<PrivatePkcs1KeyDer>, _> =
        SliceIter::new(bytes).collect();
    let rsa = rsa_result.map_err(|err| ClientError::Certificate(err.to_string()))?;
    if let Some(key) = rsa.into_iter().next() {
        return Ok(key.into());
    }

    PrivateKeyDer::try_from(bytes.to_vec()).map_err(|_| {
        ClientError::Certificate("no usable private key found in client key material".to_string())
    })
}

async fn write_once(mut stream: SendStream, payload: Vec<u8>) -> Result<SendStream> {
    if !payload.is_empty() {
        stream
            .write_all(&payload)
            .await
            .map_err(ClientError::Write)?;
    }
    Ok(stream)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn process_builder_validates_signature() {
        let signature = AbiSignature::new(Vec::new(), Vec::new());
        let builder = ProcessBuilder::new("module", "entry")
            .signature(signature.clone())
            .arg_resource(7u64);

        let result = builder.build_request();
        assert!(matches!(result, Err(ClientError::InvalidArgument(_))));
    }

    #[test]
    fn request_round_trips() {
        let req = Request::ChannelCreate(64 * 1024);
        let encoded = encode_rkyv(&req).expect("encode");
        let decoded = decode_rkyv::<Request>(&encoded).expect("decode");
        match decoded {
            Request::ChannelCreate(capacity) => assert_eq!(capacity, 64 * 1024),
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn try_parse_frame_requires_complete_prefix() {
        let mut buffer = VecDeque::from([0u8, 1u8, 2u8]);
        let frame = try_parse_frame(&mut buffer, 8, "frame length exceeds subscribed chunk size")
            .expect("parse result");
        assert!(frame.is_none());
        assert_eq!(buffer.len(), 3);
    }

    #[test]
    fn try_parse_frame_extracts_payload() {
        let mut buffer = VecDeque::new();
        buffer.extend([3u8, 0, 0, 0]); // length prefix = 3
        buffer.extend([1u8, 2, 3]);

        let frame = try_parse_frame(&mut buffer, 8, "frame length exceeds subscribed chunk size")
            .expect("parse result")
            .expect("frame available");

        assert_eq!(frame, vec![1u8, 2, 3]);
        assert!(buffer.is_empty());
    }

    #[test]
    fn try_parse_frame_rejects_oversized_frame() {
        let mut buffer = VecDeque::from([9u8, 0, 0, 0, 1, 2, 3]);
        let result = try_parse_frame(&mut buffer, 4, "frame length exceeds subscribed chunk size");
        assert!(matches!(result, Err(ClientError::InvalidArgument(_))));
    }
}
