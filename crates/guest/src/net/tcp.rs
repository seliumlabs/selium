//! TCP byte-stream socket handles over shared-memory ring buffers.
//!
//! `TcpStream` implements `tokio::io::AsyncRead + AsyncWrite` by delegating to
//! the shared [`ByteStream`] over a two-ring
//! multi-memory region. Frame headers are stripped / prepended transparently
//! so the caller sees a continuous byte stream.

use std::{
    net::SocketAddr,
    pin::Pin,
    task::{Context, Poll},
};

use selium_abi::{HostcallOutput, HostcallRequest, SharedRegionDescriptor};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

use crate::{
    Accept, GuestError, IncomingConnection, Result, hostcall::hostcall_async,
    net::bytes::ByteStream,
};

/// A TCP byte stream backed by shared-memory ring buffers.
///
/// Implements [`AsyncRead`] and [`AsyncWrite`] with correct waker registration
/// via `register_generation_wait`. Can be wrapped in `hyper_util::rt::TokioIo`
/// for BYO-framework use.
///
/// The byte-channel machinery is shared with the QUIC byte channels via the
/// internal [`ByteStream`] helper (one layout, one implementation).
pub struct TcpStream {
    inner: ByteStream,
}

/// A TCP listener that yields [`TcpStream`] handles for incoming connections.
///
/// Wraps a [`ResourceListener`](crate::ResourceListener) over a host-mediated
/// queue; the kernel accept loop pushes connection `shared_id`s onto this queue.
pub struct TcpListener {
    listener: crate::ResourceListener,
}

impl TcpStream {
    /// Connects to a remote TCP endpoint identified by an IP-literal address.
    ///
    /// The address is validated early for ergonomics; the runtime is the
    /// enforcement point for literals-only addressing.
    pub async fn connect(addr: &str) -> Result<Self> {
        // Early validation — the runtime rejects names with MalformedPayload.
        let _: SocketAddr = addr
            .parse()
            .map_err(|_e| GuestError::Host(format!("invalid IP literal address: {addr}")))?;

        let output = hostcall_async(HostcallRequest::TcpConnect {
            address: addr.to_string(),
        })
        .await?;

        let descriptor = match output {
            HostcallOutput::SharedRegion(d) => d,
            _ => return Err(GuestError::UnexpectedHostcallOutput),
        };

        Self::attach_region(descriptor)
    }

    /// Attaches to a stream region by `shared_id` (used by both `connect` and `accept`).
    fn attach_region(descriptor: SharedRegionDescriptor) -> Result<Self> {
        Ok(Self {
            inner: ByteStream::attach_descriptor(descriptor)?,
        })
    }
}

impl AsyncRead for TcpStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_read(cx, buf)
    }
}

impl AsyncWrite for TcpStream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.get_mut().inner).poll_write(cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
    }
}

impl Accept for TcpStream {
    type Item = Self;

    fn accept(connection: IncomingConnection) -> Result<Self> {
        let descriptor = SharedRegionDescriptor {
            shared_id: connection.shared_id,
            len: 0, // length not needed for attachment; header carries capacity
        };
        Self::attach_region(descriptor)
    }
}

// SAFETY: ByteStream is safe to send across threads (it encapsulates shared
// memory regions which are backed by process-level mappings).
unsafe impl Send for TcpStream {}

impl TcpListener {
    /// Binds to an IP-literal address and returns a listener.
    ///
    /// The address is validated early for ergonomics; the runtime is the
    /// enforcement point.
    pub fn bind(addr: &str) -> Result<Self> {
        let _: SocketAddr = addr
            .parse()
            .map_err(|_e| GuestError::Host(format!("invalid IP literal address: {addr}")))?;

        let output = crate::hostcall::hostcall_ready(HostcallRequest::TcpBind {
            address: addr.to_string(),
        })?;

        let descriptor = match output {
            HostcallOutput::HostQueue(d) => d,
            _ => return Err(GuestError::UnexpectedHostcallOutput),
        };

        Ok(Self {
            listener: crate::ResourceListener::from_queue(descriptor),
        })
    }

    /// Accepts the next incoming connection, returning a [`TcpStream`].
    pub async fn accept(&self) -> Result<TcpStream> {
        self.listener.accept::<TcpStream>().await
    }
}
