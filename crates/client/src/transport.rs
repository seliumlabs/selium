//! QUIC stream adapter implementing [`MessageTransport`].
//!
//! One [`QuicTransport`] owns at most one half of a bidirectional QUIC stream.
//! The read path forwards to [`quinn::RecvStream`] and the write path to
//! [`quinn::SendStream`]; the unused half is stubbed. Handles that must keep
//! the bridge relay alive in both directions (a publisher that never reads, a
//! subscriber that never writes) hold **both** halves in a single transport
//! via [`QuicTransport::bi`], so neither half is dropped (dropping a half
//! would signal teardown to the bridge).

use std::{
    io,
    pin::Pin,
    task::{Context, Poll},
};

use selium_wire::{MessageTransport, Result as WireResult};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

/// A [`MessageTransport`] over one side (or both sides) of a QUIC stream.
pub struct QuicTransport {
    recv: Option<quinn::RecvStream>,
    send: Option<quinn::SendStream>,
}

impl QuicTransport {
    /// Wraps both halves of a bidirectional stream.
    pub fn bi(send: quinn::SendStream, recv: quinn::RecvStream) -> Self {
        Self {
            recv: Some(recv),
            send: Some(send),
        }
    }

    /// Wraps only the send half (write path).
    pub fn write_only(send: quinn::SendStream) -> Self {
        Self {
            recv: None,
            send: Some(send),
        }
    }

    /// Wraps only the receive half (read path).
    pub fn read_only(recv: quinn::RecvStream) -> Self {
        Self {
            recv: Some(recv),
            send: None,
        }
    }

    /// Consumes the transport, returning its halves.
    pub fn into_parts(self) -> (Option<quinn::SendStream>, Option<quinn::RecvStream>) {
        (self.send, self.recv)
    }
}

impl AsyncRead for QuicTransport {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        match &mut self.get_mut().recv {
            Some(recv) => AsyncRead::poll_read(Pin::new(recv), cx, buf),
            None => Poll::Ready(Err(io::ErrorKind::Unsupported.into())),
        }
    }
}

impl AsyncWrite for QuicTransport {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        match &mut self.get_mut().send {
            Some(send) => AsyncWrite::poll_write(Pin::new(send), cx, buf),
            None => Poll::Ready(Err(io::ErrorKind::Unsupported.into())),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match &mut self.get_mut().send {
            Some(send) => AsyncWrite::poll_flush(Pin::new(send), cx),
            None => Poll::Ready(Ok(())),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match &mut self.get_mut().send {
            Some(send) => AsyncWrite::poll_shutdown(Pin::new(send), cx),
            None => Poll::Ready(Ok(())),
        }
    }
}

impl MessageTransport for QuicTransport {
    type Error = io::Error;

    fn poll_ready(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<WireResult<bool>> {
        // Readiness is determined by the codec against the QUIC stream; the
        // transport itself is always writable from the codec's perspective.
        Poll::Ready(Ok(true))
    }

    fn poll_peer_closed(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<WireResult<bool>> {
        // Stream end-of-input is observed as EOF on the read path; the tokio
        // read paths never call this.
        Poll::Ready(Ok(false))
    }

    fn generation(&self) -> WireResult<u64> {
        Ok(0)
    }
}
