//! Selium transport-agnostic wire framing and messaging patterns.
//!
//! This crate defines:
//!
//! - [`MessageTransport`]: a duplex framed I/O trait composing
//!   `tokio::io::AsyncRead + AsyncWrite` with readiness, peer-closed, and
//!   generation side-channels.
//! - [`FramedRead`]/[`FramedWrite`]: frame-level wrappers over any
//!   `MessageTransport`.
//! - [`Publisher`]/[`Subscriber`]: typed pub/sub handles generic over the
//!   transport.
//! - [`RpcClient`]/[`RpcConnection`]: typed request/reply handles generic over
//!   the transport.
//! - [`Rendezvous`]: connection-establishment abstraction.
//! - [`LiveTable`]: a materialised table projected from a pub/sub stream.

use std::{
    io,
    pin::Pin,
    task::{Context, Poll},
};

pub use control::{PipeControl, TERMINATE_ATTACH_FAILED, TERMINATE_BAD_HANDSHAKE};
pub use error::{Error, Result};
pub use framed::{FrameCodec, FramedRead, FramedWrite};
pub use pubsub::{Publisher, Subscriber};
pub use rpc::{Rendezvous, RpcClient, RpcConnection, RpcError, RpcRequest};
pub use stream::{
    BidiReceiver, BidiRequestStream, BidiResponder, BidiSender, RpcBidiStream, RpcBidiStreamClient,
    RpcBidiStreamConnection, RpcBidiStreamRequest, RpcServerStream, RpcServerStreamClient,
    RpcServerStreamConnection, RpcServerStreamRequest,
};
pub use tables::{
    LiveTable, LiveTableMessage, LiveTableRecord, LiveTableView, TableWriterArbiter,
    TableWriterAuthority,
};

pub mod control;
pub mod error;
pub mod framed;
pub mod pubsub;
pub mod rpc;
pub mod stream;
pub mod tables;

/// A duplex framed I/O transport.
///
/// Composes `AsyncRead + AsyncWrite + Unpin` and adds transport-specific
/// side channels for readiness, peer-closed detection, and generation
/// tracking.
pub trait MessageTransport: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin {
    /// Error type returned by transport operations.
    type Error: std::error::Error + From<io::Error>;

    /// Returns `Poll::Ready(Ok(true))` if a complete frame is immediately
    /// readable.
    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<bool>>;

    /// Returns `Poll::Ready(Ok(true))` if the remote peer has disconnected.
    fn poll_peer_closed(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<bool>>;

    /// Returns whether the peer has been explicitly marked departed.
    ///
    /// Separate from [`poll_peer_closed`](Self::poll_peer_closed), which also
    /// treats a zero writer count as close: this only reports the host's
    /// explicit signal, so a receiver can tell a confirmed death from an
    /// early handshake-time writer-count dip.
    fn peer_closed_flag(&self) -> Result<bool> {
        Ok(false)
    }

    /// Returns the current generation counter, or zero if unsupported.
    fn generation(&self) -> Result<u64>;

    /// Returns the shared region id for generation-wait registration, or 0
    /// if this transport is not backed by a shared memory region.
    fn region_id(&self) -> u64 {
        0
    }

    /// Returns the shared arbiter used to coordinate a live-table writer, or
    /// `None` when this transport cannot enforce cross-writer CAS. Transports
    /// without a real shared arbiter must not substitute a local topic string
    /// and pretend to offer distributed atomicity.
    fn table_writer_arbiter(&self) -> Option<std::sync::Arc<dyn TableWriterArbiter>> {
        None
    }
}

/// Waits until the reader's generation advances past `observed_generation`,
/// or parks the current task through the generation-wait callback installed by
/// the reactor.
///
/// Falls back to a waker wake if no callback is installed. Frame, generation,
/// and EOF state are re-checked after registration so a producer that commits
/// between the caller's empty read and the registration cannot strand the task.
pub(crate) async fn generation_wait<M: MessageTransport>(
    reader: &mut FramedRead<M>,
    observed_generation: u64,
) {
    let region_id = reader.inner().region_id();
    let mut yielded = false;
    std::future::poll_fn(move |cx| {
        if yielded {
            return Poll::Ready(());
        }
        yielded = true;
        if !selium_memory::register_generation_wait(region_id, observed_generation, cx.waker()) {
            cx.waker().wake_by_ref();
            return Poll::Pending;
        }
        if reader.generation().ok() != Some(observed_generation)
            || reader.poll_ready().unwrap_or(true)
            || reader.poll_peer_closed().unwrap_or(true)
        {
            cx.waker().wake_by_ref();
        }
        Poll::Pending
    })
    .await
}
