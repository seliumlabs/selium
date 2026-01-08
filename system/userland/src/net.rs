//! Guest-facing helpers for establishing and servicing network connections.
//!
//! Network access is mediated by Selium capabilities and exposed to guests via hostcalls.
//!
//! # Examples
//! ```no_run
//! fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     let rt = tokio::runtime::Builder::new_current_thread().build()?;
//!     rt.block_on(async {
//!         let mut conn =
//!             selium_userland::net::connect(selium_userland::net::NetProtocol::Quic, "example.com", 443)
//!                 .await?;
//!         conn.send(b"hello").await?;
//!         let _maybe_frame = conn.recv().await?;
//!         Ok::<_, selium_userland::net::NetError>(())
//!     })?;
//!     Ok(())
//! }
//! ```

use core::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};
use std::{
    fmt::{Debug, Formatter},
    task::ready,
};

use futures::{Sink, SinkExt, Stream, StreamExt};
use selium_abi::{
    GuestResourceId, GuestUint, IoFrame, IoRead, IoWrite, NetAccept, NetAcceptReply, NetConnect,
    NetConnectReply, NetCreateListener, NetCreateListenerReply, NetTlsClientConfig,
    NetTlsConfigReply, NetTlsServerConfig,
};

use crate::{
    FromHandle,
    driver::{DriverError, DriverFuture, RKYV_VEC_OVERHEAD, RkyvDecoder, encode_args},
};

/// Network protocol identifiers supported by the userland helpers.
pub use selium_abi::NetProtocol;
/// TLS material supplied by a guest for client connections.
pub use selium_abi::TlsClientBundle;
/// TLS material supplied by a guest for server listeners.
pub use selium_abi::TlsServerBundle;

/// Error returned by network helpers.
pub type NetError = DriverError;
/// Raw frame yielded by network readers.
pub type Frame = IoFrame;
type AcceptFuture = Pin<Box<dyn Future<Output = Result<NetAcceptReply, NetError>> + 'static>>;
type FrameReadFuture = Pin<Box<dyn Future<Output = Result<IoFrame, NetError>> + 'static>>;
type WriteFuture = Pin<Box<dyn Future<Output = Result<GuestUint, NetError>> + 'static>>;

/// Maximum reply size for control-plane hostcalls.
const NET_REPLY_CAPACITY: usize = 256;
const DEFAULT_CHUNK_SIZE: usize = 16 * 1024;

/// TLS configuration handle for server listeners.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TlsServerConfig {
    handle: GuestResourceId,
}

/// TLS configuration handle for client connections.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TlsClientConfig {
    handle: GuestResourceId,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ListenerInner {
    handle: GuestResourceId,
    protocol: NetProtocol,
}

/// QUIC network listener bound to a domain and port.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct QuicListener {
    inner: ListenerInner,
}

/// HTTP network listener bound to a domain and port.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HttpListener {
    inner: ListenerInner,
}

/// HTTPS network listener bound to a domain and port.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HttpsListener {
    inner: ListenerInner,
}

/// Stream of inbound connections for a [`QuicListener`], [`HttpListener`], or [`HttpsListener`].
pub struct Incoming {
    handle: GuestResourceId,
    chunk: usize,
    protocol: NetProtocol,
    inflight: Option<AcceptFuture>,
}

/// Bidirectional network connection.
pub struct Connection {
    reader: Reader,
    writer: Writer,
    remote_addr: String,
}

/// Reader side of a network connection.
pub struct Reader {
    handle: GuestUint,
    chunk: usize,
    protocol: NetProtocol,
    inflight: Option<FrameReadFuture>,
}

/// Writer side of a network connection.
pub struct Writer {
    handle: GuestUint,
    protocol: NetProtocol,
    inflight: Option<WriteFuture>,
}

struct ConnectionHandles {
    reader: GuestResourceId,
    writer: GuestResourceId,
    remote_addr: String,
}

impl From<NetConnectReply> for ConnectionHandles {
    fn from(value: NetConnectReply) -> Self {
        Self {
            reader: value.reader,
            writer: value.writer,
            remote_addr: value.remote_addr,
        }
    }
}

impl From<NetAcceptReply> for ConnectionHandles {
    fn from(value: NetAcceptReply) -> Self {
        Self {
            reader: value.reader,
            writer: value.writer,
            remote_addr: value.remote_addr,
        }
    }
}

impl ListenerInner {
    /// Bind to a domain and port using the selected protocol, returning a listener handle.
    async fn bind(
        protocol: NetProtocol,
        domain: &str,
        port: u16,
        tls: Option<&TlsServerConfig>,
    ) -> Result<Self, NetError> {
        ensure_supported(protocol)?;
        if matches!(protocol, NetProtocol::Http) && tls.is_some() {
            return Err(NetError::InvalidArgument);
        }
        let args = NetCreateListener {
            protocol,
            domain: domain.to_string(),
            port,
            tls: tls.map(|config| config.handle),
        };
        let encoded = encode_args(&args)?;
        let reply = match protocol {
            NetProtocol::Quic => {
                DriverFuture::<net_quic_bind::Module, RkyvDecoder<NetCreateListenerReply>>::new(
                    &encoded,
                    NET_REPLY_CAPACITY,
                    RkyvDecoder::new(),
                )?
                .await?
            }
            NetProtocol::Http | NetProtocol::Https => {
                DriverFuture::<net_http_bind::Module, RkyvDecoder<NetCreateListenerReply>>::new(
                    &encoded,
                    NET_REPLY_CAPACITY,
                    RkyvDecoder::new(),
                )?
                .await?
            }
        };
        Ok(Self {
            handle: reply.handle,
            protocol,
        })
    }

    /// Accept a single inbound connection.
    async fn accept(&self) -> Result<Connection, NetError> {
        let reply = accept_once(self.protocol, self.handle).await?;
        connection_from_reply(self.protocol, reply, DEFAULT_CHUNK_SIZE)
    }

    /// Iterate over inbound connections as a stream.
    fn incoming(&self) -> Incoming {
        Incoming {
            handle: self.handle,
            chunk: DEFAULT_CHUNK_SIZE,
            protocol: self.protocol,
            inflight: None,
        }
    }

    /// Expose the underlying registry handle.
    fn handle(&self) -> GuestResourceId {
        self.handle
    }
}

impl QuicListener {
    /// Bind to a domain and port using QUIC, returning a listener handle.
    pub async fn bind(domain: &str, port: u16) -> Result<Self, NetError> {
        let inner = ListenerInner::bind(NetProtocol::Quic, domain, port, None).await?;
        Ok(Self { inner })
    }

    /// Bind to a domain and port using QUIC with a custom TLS configuration.
    pub async fn bind_with_tls(
        domain: &str,
        port: u16,
        tls: &TlsServerConfig,
    ) -> Result<Self, NetError> {
        let inner = ListenerInner::bind(NetProtocol::Quic, domain, port, Some(tls)).await?;
        Ok(Self { inner })
    }

    /// Accept a single inbound QUIC connection.
    pub async fn accept(&self) -> Result<Connection, NetError> {
        self.inner.accept().await
    }

    /// Iterate over inbound QUIC connections as a stream.
    pub fn incoming(&self) -> Incoming {
        self.inner.incoming()
    }

    /// Expose the underlying registry handle.
    pub fn handle(&self) -> GuestResourceId {
        self.inner.handle()
    }
}

impl HttpListener {
    /// Bind to a domain and port using HTTP, returning a listener handle.
    pub async fn bind(domain: &str, port: u16) -> Result<Self, NetError> {
        let inner = ListenerInner::bind(NetProtocol::Http, domain, port, None).await?;
        Ok(Self { inner })
    }

    /// Accept a single inbound HTTP connection.
    pub async fn accept(&self) -> Result<Connection, NetError> {
        self.inner.accept().await
    }

    /// Iterate over inbound HTTP connections as a stream.
    pub fn incoming(&self) -> Incoming {
        self.inner.incoming()
    }

    /// Expose the underlying registry handle.
    pub fn handle(&self) -> GuestResourceId {
        self.inner.handle()
    }
}

impl HttpsListener {
    /// Bind to a domain and port using HTTPS, returning a listener handle.
    pub async fn bind(domain: &str, port: u16) -> Result<Self, NetError> {
        let inner = ListenerInner::bind(NetProtocol::Https, domain, port, None).await?;
        Ok(Self { inner })
    }

    /// Bind to a domain and port using HTTPS with a custom TLS configuration.
    pub async fn bind_with_tls(
        domain: &str,
        port: u16,
        tls: &TlsServerConfig,
    ) -> Result<Self, NetError> {
        let inner = ListenerInner::bind(NetProtocol::Https, domain, port, Some(tls)).await?;
        Ok(Self { inner })
    }

    /// Accept a single inbound HTTPS connection.
    pub async fn accept(&self) -> Result<Connection, NetError> {
        self.inner.accept().await
    }

    /// Iterate over inbound HTTPS connections as a stream.
    pub fn incoming(&self) -> Incoming {
        self.inner.incoming()
    }

    /// Expose the underlying registry handle.
    pub fn handle(&self) -> GuestResourceId {
        self.inner.handle()
    }
}

impl Incoming {
    /// Override the chunk size used by accepted readers.
    pub fn with_chunk_size(mut self, chunk: usize) -> Self {
        self.chunk = chunk.max(1);
        self
    }
}

impl Stream for Incoming {
    type Item = Result<Connection, NetError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let incoming = self.get_mut();

        if incoming.inflight.is_none() {
            match accept_future(incoming.protocol, incoming.handle) {
                Ok(fut) => incoming.inflight = Some(fut),
                Err(err) => return Poll::Ready(Some(Err(err))),
            }
        }

        let fut = match incoming.inflight.as_mut() {
            Some(fut) => fut,
            None => return Poll::Ready(Some(Err(NetError::InvalidArgument))),
        };

        match fut.as_mut().poll(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(result) => {
                incoming.inflight = None;
                Poll::Ready(Some(result.and_then(|reply| {
                    connection_from_reply(incoming.protocol, reply, incoming.chunk)
                })))
            }
        }
    }
}

impl Connection {
    /// Split the connection into owned reader and writer halves.
    pub fn split(self) -> (Reader, Writer) {
        (self.reader, self.writer)
    }

    /// Borrow the reader and writer halves without consuming the connection.
    pub fn borrow_split(&mut self) -> (&mut Reader, &mut Writer) {
        (&mut self.reader, &mut self.writer)
    }

    /// Receive the next frame from the remote peer.
    pub async fn recv(&mut self) -> Result<Option<Frame>, NetError> {
        self.reader.next().await.transpose()
    }

    /// Send a frame to the remote peer.
    pub async fn send(&mut self, payload: impl AsRef<[u8]>) -> Result<(), NetError> {
        SinkExt::send(&mut self.writer, payload.as_ref().to_vec()).await
    }

    /// Override the chunk size used by this connection's reader.
    pub fn with_chunk_size(mut self, chunk: usize) -> Self {
        self.reader = self.reader.with_chunk_size(chunk);
        self
    }

    /// Access the reader half without consuming the connection.
    pub fn reader(&mut self) -> &mut Reader {
        &mut self.reader
    }

    /// Access the writer half without consuming the connection.
    pub fn writer(&mut self) -> &mut Writer {
        &mut self.writer
    }
}

impl Debug for Connection {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.remote_addr)
    }
}

impl Reader {
    /// Override the chunk size used for each read.
    pub fn with_chunk_size(mut self, chunk: usize) -> Self {
        self.chunk = chunk.max(1);
        self
    }

    /// Retrieve the raw reader handle.
    pub fn handle(&self) -> GuestUint {
        self.handle
    }

    fn poll_frame(&mut self, len: usize, cx: &mut Context<'_>) -> Poll<Result<Frame, NetError>> {
        if len == 0 {
            return Poll::Ready(Err(NetError::InvalidArgument));
        }

        if self.inflight.is_none() {
            if let Err(err) = ensure_supported(self.protocol) {
                return Poll::Ready(Err(err));
            }
            let len32 = match GuestUint::try_from(len) {
                Ok(v) => v,
                Err(_) => return Poll::Ready(Err(NetError::InvalidArgument)),
            };
            let args = IoRead {
                handle: self.handle,
                len: len32,
            };
            let encoded = match encode_args(&args) {
                Ok(bytes) => bytes,
                Err(err) => return Poll::Ready(Err(err)),
            };
            let fut = match read_future(self.protocol, &encoded, len) {
                Ok(fut) => fut,
                Err(err) => return Poll::Ready(Err(err)),
            };
            self.inflight = Some(fut);
        }

        let fut = match self.inflight.as_mut() {
            Some(fut) => fut,
            None => return Poll::Ready(Err(NetError::InvalidArgument)),
        };

        match fut.as_mut().poll(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(res) => {
                self.inflight = None;
                Poll::Ready(res)
            }
        }
    }
}

impl Stream for Reader {
    type Item = Result<Frame, NetError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let reader = self.get_mut();
        match reader.poll_frame(reader.chunk, cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Ok(frame)) => {
                if frame.payload.is_empty() {
                    Poll::Ready(None)
                } else {
                    Poll::Ready(Some(Ok(frame)))
                }
            }
            Poll::Ready(Err(err)) => Poll::Ready(Some(Err(err))),
        }
    }
}

impl Writer {
    /// Retrieve the raw writer handle.
    pub fn handle(&self) -> GuestUint {
        self.handle
    }
}

impl Sink<Vec<u8>> for Writer {
    type Error = NetError;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        let this = self.get_mut();
        match this.inflight.as_mut() {
            Some(fut) => match fut.as_mut().poll(cx) {
                Poll::Pending => Poll::Pending,
                Poll::Ready(result) => {
                    this.inflight = None;
                    Poll::Ready(result.map(|_| ()))
                }
            },
            None => Poll::Ready(Ok(())),
        }
    }

    fn start_send(mut self: Pin<&mut Self>, payload: Vec<u8>) -> Result<(), Self::Error> {
        if self.inflight.is_some() {
            return Err(NetError::InvalidArgument);
        }

        if payload.is_empty() {
            return Ok(());
        }

        ensure_supported(self.protocol)?;
        let args = IoWrite {
            handle: self.handle,
            payload,
        };
        let encoded = encode_args(&args)?;
        let fut = write_future(self.protocol, &encoded)?;

        self.inflight = Some(fut);

        Ok(())
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        if let Some(fut) = self.as_mut().get_mut().inflight.as_mut() {
            ready!(fut.as_mut().poll(cx))?;
            self.as_mut().get_mut().inflight = None;
        }

        Poll::Ready(Ok(()))
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.poll_flush(cx)
    }
}

impl TlsServerConfig {
    /// Register a TLS server configuration from a bundle.
    pub async fn register(bundle: TlsServerBundle) -> Result<Self, NetError> {
        let args = NetTlsServerConfig { bundle };
        let encoded = encode_args(&args)?;
        let reply = DriverFuture::<
            net_tls_server_config_create::Module,
            RkyvDecoder<NetTlsConfigReply>,
        >::new(&encoded, NET_REPLY_CAPACITY, RkyvDecoder::new())?
        .await?;
        Ok(Self {
            handle: reply.handle,
        })
    }

    /// Expose the underlying registry handle.
    pub fn handle(&self) -> GuestResourceId {
        self.handle
    }
}

impl TlsClientConfig {
    /// Register a TLS client configuration from a bundle.
    pub async fn register(bundle: TlsClientBundle) -> Result<Self, NetError> {
        let args = NetTlsClientConfig { bundle };
        let encoded = encode_args(&args)?;
        let reply = DriverFuture::<
            net_tls_client_config_create::Module,
            RkyvDecoder<NetTlsConfigReply>,
        >::new(&encoded, NET_REPLY_CAPACITY, RkyvDecoder::new())?
        .await?;
        Ok(Self {
            handle: reply.handle,
        })
    }

    /// Expose the underlying registry handle.
    pub fn handle(&self) -> GuestResourceId {
        self.handle
    }
}

impl FromHandle for TlsServerConfig {
    type Handles = GuestResourceId;

    unsafe fn from_handle(handle: Self::Handles) -> Self {
        Self { handle }
    }
}

impl FromHandle for TlsClientConfig {
    type Handles = GuestResourceId;

    unsafe fn from_handle(handle: Self::Handles) -> Self {
        Self { handle }
    }
}

/// Connect to a remote endpoint using the selected protocol and return a convenience wrapper.
pub async fn connect(
    protocol: NetProtocol,
    domain: &str,
    port: u16,
) -> Result<Connection, NetError> {
    let reply = connect_raw_with_tls(protocol, domain, port, None).await?;
    connection_from_reply(protocol, reply, DEFAULT_CHUNK_SIZE)
}

/// Connect to a remote endpoint using the selected protocol and custom TLS config.
pub async fn connect_with_tls(
    protocol: NetProtocol,
    domain: &str,
    port: u16,
    tls: &TlsClientConfig,
) -> Result<Connection, NetError> {
    let reply = connect_raw_with_tls(protocol, domain, port, Some(tls)).await?;
    connection_from_reply(protocol, reply, DEFAULT_CHUNK_SIZE)
}

/// Connect to a remote endpoint using the selected protocol, returning raw reader and writer handles.
pub async fn connect_raw(
    protocol: NetProtocol,
    domain: &str,
    port: u16,
) -> Result<NetConnectReply, NetError> {
    connect_raw_with_tls(protocol, domain, port, None).await
}

/// Connect to a remote endpoint using the selected protocol and custom TLS config.
pub async fn connect_raw_with_tls(
    protocol: NetProtocol,
    domain: &str,
    port: u16,
    tls: Option<&TlsClientConfig>,
) -> Result<NetConnectReply, NetError> {
    ensure_supported(protocol)?;
    if matches!(protocol, NetProtocol::Http) && tls.is_some() {
        return Err(NetError::InvalidArgument);
    }
    let args = NetConnect {
        protocol,
        domain: domain.to_string(),
        port,
        tls: tls.map(|config| config.handle),
    };
    let encoded = encode_args(&args)?;
    match protocol {
        NetProtocol::Quic => {
            DriverFuture::<net_quic_connect::Module, RkyvDecoder<NetConnectReply>>::new(
                &encoded,
                NET_REPLY_CAPACITY,
                RkyvDecoder::new(),
            )?
            .await
        }
        NetProtocol::Http | NetProtocol::Https => {
            DriverFuture::<net_http_connect::Module, RkyvDecoder<NetConnectReply>>::new(
                &encoded,
                NET_REPLY_CAPACITY,
                RkyvDecoder::new(),
            )?
            .await
        }
    }
}

fn accept_future(protocol: NetProtocol, handle: GuestResourceId) -> Result<AcceptFuture, NetError> {
    ensure_supported(protocol)?;
    let args = NetAccept { handle };
    let encoded = encode_args(&args)?;
    accept_future_with_args(protocol, &encoded)
}

async fn accept_once(
    protocol: NetProtocol,
    handle: GuestResourceId,
) -> Result<NetAcceptReply, NetError> {
    accept_future(protocol, handle)?.await
}

fn connection_from_reply(
    protocol: NetProtocol,
    reply: impl Into<ConnectionHandles>,
    chunk: usize,
) -> Result<Connection, NetError> {
    let handles = reply.into();
    let reader = Reader {
        handle: guest_handle(handles.reader)?,
        chunk,
        protocol,
        inflight: None,
    };
    let writer = Writer {
        handle: guest_handle(handles.writer)?,
        protocol,
        inflight: None,
    };

    Ok(Connection {
        reader,
        writer,
        remote_addr: handles.remote_addr,
    })
}

fn guest_handle(handle: GuestResourceId) -> Result<GuestUint, NetError> {
    GuestUint::try_from(handle).map_err(|_| NetError::InvalidArgument)
}

fn ensure_supported(protocol: NetProtocol) -> Result<(), NetError> {
    match protocol {
        NetProtocol::Quic | NetProtocol::Http | NetProtocol::Https => Ok(()),
    }
}

fn read_future(
    protocol: NetProtocol,
    encoded: &[u8],
    len: usize,
) -> Result<FrameReadFuture, NetError> {
    match protocol {
        NetProtocol::Quic => {
            let fut = DriverFuture::<net_quic_read::Module, RkyvDecoder<Frame>>::new(
                encoded,
                len + RKYV_VEC_OVERHEAD + 8,
                RkyvDecoder::new(),
            )?;
            Ok(Box::pin(fut))
        }
        NetProtocol::Http | NetProtocol::Https => {
            let fut = DriverFuture::<net_http_read::Module, RkyvDecoder<Frame>>::new(
                encoded,
                len + RKYV_VEC_OVERHEAD + 8,
                RkyvDecoder::new(),
            )?;
            Ok(Box::pin(fut))
        }
    }
}

fn write_future(protocol: NetProtocol, encoded: &[u8]) -> Result<WriteFuture, NetError> {
    match protocol {
        NetProtocol::Quic => {
            let fut = DriverFuture::<net_quic_write::Module, RkyvDecoder<GuestUint>>::new(
                encoded,
                8,
                RkyvDecoder::new(),
            )?;
            Ok(Box::pin(fut))
        }
        NetProtocol::Http | NetProtocol::Https => {
            let fut = DriverFuture::<net_http_write::Module, RkyvDecoder<GuestUint>>::new(
                encoded,
                8,
                RkyvDecoder::new(),
            )?;
            Ok(Box::pin(fut))
        }
    }
}

fn accept_future_with_args(
    protocol: NetProtocol,
    encoded: &[u8],
) -> Result<AcceptFuture, NetError> {
    match protocol {
        NetProtocol::Quic => {
            let fut = DriverFuture::<net_quic_accept::Module, RkyvDecoder<NetAcceptReply>>::new(
                encoded,
                NET_REPLY_CAPACITY,
                RkyvDecoder::new(),
            )?;
            Ok(Box::pin(fut))
        }
        NetProtocol::Http | NetProtocol::Https => {
            let fut = DriverFuture::<net_http_accept::Module, RkyvDecoder<NetAcceptReply>>::new(
                encoded,
                NET_REPLY_CAPACITY,
                RkyvDecoder::new(),
            )?;
            Ok(Box::pin(fut))
        }
    }
}

driver_module!(net_quic_bind, NET_QUIC_BIND, "selium::net::quic::bind");
driver_module!(
    net_quic_accept,
    NET_QUIC_ACCEPT,
    "selium::net::quic::accept"
);
driver_module!(
    net_quic_connect,
    NET_QUIC_CONNECT,
    "selium::net::quic::connect"
);
driver_module!(net_quic_read, NET_QUIC_READ, "selium::net::quic::read");
driver_module!(net_quic_write, NET_QUIC_WRITE, "selium::net::quic::write");
driver_module!(net_http_bind, NET_HTTP_BIND, "selium::net::http::bind");
driver_module!(
    net_http_accept,
    NET_HTTP_ACCEPT,
    "selium::net::http::accept"
);
driver_module!(
    net_http_connect,
    NET_HTTP_CONNECT,
    "selium::net::http::connect"
);
driver_module!(net_http_read, NET_HTTP_READ, "selium::net::http::read");
driver_module!(net_http_write, NET_HTTP_WRITE, "selium::net::http::write");
driver_module!(
    net_tls_server_config_create,
    NET_TLS_SERVER_CONFIG_CREATE,
    "selium::net::tls::server_config_create"
);
driver_module!(
    net_tls_client_config_create,
    NET_TLS_CLIENT_CONFIG_CREATE,
    "selium::net::tls::client_config_create"
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunk_sizes_clamp_to_one() {
        let incoming = QuicListener {
            inner: ListenerInner {
                handle: 1,
                protocol: NetProtocol::Quic,
            },
        }
        .incoming()
        .with_chunk_size(0);
        assert_eq!(incoming.chunk, 1);

        let reader = Reader {
            handle: 1,
            chunk: 4,
            protocol: NetProtocol::Quic,
            inflight: None,
        }
        .with_chunk_size(0);
        assert_eq!(reader.chunk, 1);
    }

    #[test]
    fn guest_handle_rejects_overflow() {
        let result = guest_handle(u64::from(u32::MAX) + 1);
        assert!(matches!(result, Err(NetError::InvalidArgument)));
    }

    #[test]
    fn connection_builder_downcasts_handles() {
        let reply = NetConnectReply {
            reader: 5,
            writer: 7,
            remote_addr: "localhost:123".into(),
        };
        let conn = connection_from_reply(NetProtocol::Quic, reply, 8).expect("connection");
        assert_eq!(conn.reader.handle(), 5);
        assert_eq!(conn.writer.handle(), 7);
    }
}
