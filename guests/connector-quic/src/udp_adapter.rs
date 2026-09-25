//! quinn `AsyncUdpSocket` + `UdpPoller` adapters over the guest `UdpSocket`.
//!
//! quinn never talks to a real OS socket on wasm32: the connector guest holds
//! a shared-memory-backed [`UdpSocket`] (send/recv rings bridged to the host's
//! UDP socket by the runtime), and this adapter maps quinn's datagram surface
//! onto it:
//!
//! - `try_send`: encode a [`Datagram`] onto the send ring, or report
//!   `WouldBlock` when the ring is pinned full (backpressure).
//! - `poll_recv`: decode the next [`Datagram`] from the recv ring into quinn
//!   `RecvMeta` + caller buffers.
//! - `create_io_poller`: a [`UdpPoller`] that parks on the send ring's
//!   generation counter when the ring is full, so quinn retries `try_send`
//!   exactly when a draining peer frees capacity.
//!
//! ECN is degraded (fixed `Ect0`) and there is no GSO over the shm frames —
//! both are acceptable for v1 and noted in the design.

use std::{
    fmt,
    io::{self, IoSliceMut},
    net::SocketAddr,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use parking_lot::Mutex;
use quinn::{
    AsyncUdpSocket, UdpPoller,
    udp::{EcnCodepoint, RecvMeta, Transmit},
};
use selium_guest::{Datagram, UdpSocket};

/// A quinn `AsyncUdpSocket` over the guest's shared-memory [`UdpSocket`].
pub struct QuicUdpSocket {
    inner: Arc<Mutex<UdpSocket>>,
    local_addr: SocketAddr,
}

/// A quinn `UdpPoller` that reports the associated socket's send readiness.
pub struct QuicUdpPoller {
    socket: Arc<QuicUdpSocket>,
}

impl QuicUdpSocket {
    /// Wraps a bound guest [`UdpSocket`] for quinn.
    pub fn new(socket: UdpSocket, local_addr: SocketAddr) -> Self {
        Self {
            inner: Arc::new(Mutex::new(socket)),
            local_addr,
        }
    }
}

impl AsyncUdpSocket for QuicUdpSocket {
    fn create_io_poller(self: Arc<Self>) -> Pin<Box<dyn UdpPoller>> {
        Box::pin(QuicUdpPoller { socket: self })
    }

    fn try_send(&self, transmit: &Transmit) -> io::Result<()> {
        let datagram = Datagram {
            addr: transmit.destination,
            payload: transmit.contents.to_vec(),
        };

        let mut guard = self.inner.lock();
        // `try_send` is synchronous and must not await: poll once against a
        // no-op waker. A Pending result means the ring is pinned full, which
        // we surface as WouldBlock so quinn parks on the channel generation
        // via `UdpPoller::poll_writable` (see below).
        let socket = Pin::new(&mut *guard);
        let noop = std::task::Waker::noop();
        let mut cx = Context::from_waker(noop);
        match socket.poll_send(&mut cx, &datagram) {
            Poll::Ready(Ok(_)) => Ok(()),
            Poll::Ready(Err(e)) => Err(io::Error::other(e)),
            Poll::Pending => Err(io::ErrorKind::WouldBlock.into()),
        }
    }

    fn poll_recv(
        &self,
        cx: &mut Context<'_>,
        bufs: &mut [IoSliceMut<'_>],
        meta: &mut [RecvMeta],
    ) -> Poll<io::Result<usize>> {
        let mut guard = self.inner.lock();
        let socket = Pin::new(&mut *guard);
        match socket.poll_recv(cx) {
            Poll::Ready(Ok(datagram)) => {
                let (Some(buf), Some(meta_slot)) = (bufs.first_mut(), meta.first_mut()) else {
                    return Poll::Ready(Ok(0));
                };
                let n = datagram.payload.len().min(buf.len());
                #[expect(
                    clippy::indexing_slicing,
                    reason = "n is bounded by min(buf.len(), payload.len())"
                )]
                buf[..n].copy_from_slice(&datagram.payload[..n]);
                *meta_slot = RecvMeta {
                    addr: datagram.addr,
                    len: n,
                    stride: n,
                    ecn: Some(EcnCodepoint::Ect0),
                    dst_ip: None,
                };
                Poll::Ready(Ok(1))
            }
            Poll::Ready(Err(e)) => Poll::Ready(Err(io::Error::other(e))),
            Poll::Pending => Poll::Pending,
        }
    }

    fn local_addr(&self) -> io::Result<SocketAddr> {
        Ok(self.local_addr)
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

impl fmt::Debug for QuicUdpSocket {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("QuicUdpSocket")
            .field("local_addr", &self.local_addr)
            .finish_non_exhaustive()
    }
}

impl UdpPoller for QuicUdpPoller {
    fn poll_writable(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let mut guard = self.socket.inner.lock();
        let socket = Pin::new(&mut *guard);
        match socket.poll_send_ready(cx) {
            Poll::Ready(Ok(())) => Poll::Ready(Ok(())),
            Poll::Ready(Err(e)) => Poll::Ready(Err(io::Error::other(e))),
            Poll::Pending => Poll::Pending,
        }
    }
}

impl fmt::Debug for QuicUdpPoller {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("QuicUdpPoller").finish_non_exhaustive()
    }
}
