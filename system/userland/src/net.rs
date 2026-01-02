//! Guest-facing helpers for establishing and servicing network connections.
//!
//! Network access is mediated by Selium capabilities and exposed to guests via hostcalls.
//!
//! # Examples
//! ```no_run
//! fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     let rt = tokio::runtime::Builder::new_current_thread().build()?;
//!     rt.block_on(async {
//!         let mut conn = selium_userland::net::connect("example.com", 443).await?;
//!         conn.send(b"hello").await?;
//!         let _maybe_frame = conn.recv().await?;
//!         Ok::<_, selium_userland::net::NetError>(())
//!     })?;
//!     Ok(())
//! }
//! ```

use core::{
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
    NetConnectReply, NetCreateListener, NetCreateListenerReply,
};

use crate::driver::{DriverError, DriverFuture, RKYV_VEC_OVERHEAD, RkyvDecoder, encode_args};

/// Error returned by network helpers.
pub type NetError = DriverError;
/// Raw frame yielded by network readers.
pub type Frame = IoFrame;
type AcceptFuture = DriverFuture<net_accept::Module, RkyvDecoder<NetAcceptReply>>;
type FrameReadFuture = DriverFuture<net_read::Module, RkyvDecoder<IoFrame>>;
type WriteFuture = DriverFuture<net_write::Module, RkyvDecoder<GuestUint>>;

/// Maximum reply size for control-plane hostcalls.
const NET_REPLY_CAPACITY: usize = 256;
const DEFAULT_CHUNK_SIZE: usize = 16 * 1024;

/// Network listener bound to a domain and port.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Listener {
    handle: GuestResourceId,
}

/// Stream of inbound connections for a [`Listener`].
pub struct Incoming {
    handle: GuestResourceId,
    chunk: usize,
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
    inflight: Option<FrameReadFuture>,
}

/// Writer side of a network connection.
pub struct Writer {
    handle: GuestUint,
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

impl Listener {
    /// Bind to a domain and port, returning a listener handle.
    pub async fn bind(domain: &str, port: u16) -> Result<Self, NetError> {
        let args = NetCreateListener {
            domain: domain.to_string(),
            port,
        };
        let encoded = encode_args(&args)?;
        let reply = DriverFuture::<net_bind::Module, RkyvDecoder<NetCreateListenerReply>>::new(
            &encoded,
            NET_REPLY_CAPACITY,
            RkyvDecoder::new(),
        )?
        .await?;
        Ok(Self {
            handle: reply.handle,
        })
    }

    /// Accept a single inbound connection.
    pub async fn accept(&self) -> Result<Connection, NetError> {
        let reply = accept_once(self.handle).await?;
        connection_from_reply(reply, DEFAULT_CHUNK_SIZE)
    }

    /// Iterate over inbound connections as a stream.
    pub fn incoming(&self) -> Incoming {
        Incoming {
            handle: self.handle,
            chunk: DEFAULT_CHUNK_SIZE,
            inflight: None,
        }
    }

    /// Expose the underlying registry handle.
    pub fn handle(&self) -> GuestResourceId {
        self.handle
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
            match accept_future(incoming.handle) {
                Ok(fut) => incoming.inflight = Some(fut),
                Err(err) => return Poll::Ready(Some(Err(err))),
            }
        }

        let fut = match incoming.inflight.as_mut() {
            Some(fut) => fut,
            None => return Poll::Ready(Some(Err(NetError::InvalidArgument))),
        };

        match Pin::new(fut).poll(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(result) => {
                incoming.inflight = None;
                Poll::Ready(Some(
                    result.and_then(|reply| connection_from_reply(reply, incoming.chunk)),
                ))
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
            let fut = match DriverFuture::<net_read::Module, RkyvDecoder<Frame>>::new(
                &encoded,
                len + RKYV_VEC_OVERHEAD + 8,
                RkyvDecoder::new(),
            ) {
                Ok(fut) => fut,
                Err(err) => return Poll::Ready(Err(err)),
            };
            self.inflight = Some(fut);
        }

        let fut = match self.inflight.as_mut() {
            Some(fut) => fut,
            None => return Poll::Ready(Err(NetError::InvalidArgument)),
        };

        match Pin::new(fut).poll(cx) {
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
            Some(fut) => match Pin::new(fut).poll(cx) {
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

        let args = IoWrite {
            handle: self.handle,
            payload,
        };
        let encoded = encode_args(&args)?;
        let fut = DriverFuture::<net_write::Module, RkyvDecoder<GuestUint>>::new(
            &encoded,
            8,
            RkyvDecoder::new(),
        )?;

        self.inflight = Some(fut);

        Ok(())
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        if let Some(fut) = self.as_mut().get_mut().inflight.as_mut() {
            ready!(Pin::new(fut).poll(cx))?;
            self.as_mut().get_mut().inflight = None;
        }

        Poll::Ready(Ok(()))
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.poll_flush(cx)
    }
}

/// Connect to a remote endpoint and return a convenience wrapper.
pub async fn connect(domain: &str, port: u16) -> Result<Connection, NetError> {
    let reply = connect_raw(domain, port).await?;
    connection_from_reply(reply, DEFAULT_CHUNK_SIZE)
}

/// Connect to a remote endpoint, returning raw reader and writer handles.
pub async fn connect_raw(domain: &str, port: u16) -> Result<NetConnectReply, NetError> {
    let args = NetConnect {
        domain: domain.to_string(),
        port,
    };
    let encoded = encode_args(&args)?;
    DriverFuture::<net_connect::Module, RkyvDecoder<NetConnectReply>>::new(
        &encoded,
        NET_REPLY_CAPACITY,
        RkyvDecoder::new(),
    )?
    .await
}

fn accept_future(handle: GuestResourceId) -> Result<AcceptFuture, NetError> {
    let args = NetAccept { handle };
    let encoded = encode_args(&args)?;
    DriverFuture::<net_accept::Module, RkyvDecoder<NetAcceptReply>>::new(
        &encoded,
        NET_REPLY_CAPACITY,
        RkyvDecoder::new(),
    )
}

async fn accept_once(handle: GuestResourceId) -> Result<NetAcceptReply, NetError> {
    accept_future(handle)?.await
}

fn connection_from_reply(
    reply: impl Into<ConnectionHandles>,
    chunk: usize,
) -> Result<Connection, NetError> {
    let handles = reply.into();
    let reader = Reader {
        handle: guest_handle(handles.reader)?,
        chunk,
        inflight: None,
    };
    let writer = Writer {
        handle: guest_handle(handles.writer)?,
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

driver_module!(net_bind, NET_BIND, "selium::net::bind");
driver_module!(net_accept, NET_ACCEPT, "selium::net::accept");
driver_module!(net_connect, NET_CONNECT, "selium::net::connect");
driver_module!(net_read, NET_READ, "selium::net::read");
driver_module!(net_write, NET_WRITE, "selium::net::write");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunk_sizes_clamp_to_one() {
        let incoming = Listener { handle: 1 }.incoming().with_chunk_size(0);
        assert_eq!(incoming.chunk, 1);

        let reader = Reader {
            handle: 1,
            chunk: 4,
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
        let conn = connection_from_reply(reply, 8).expect("connection");
        assert_eq!(conn.reader.handle(), 5);
        assert_eq!(conn.writer.handle(), 7);
    }
}
