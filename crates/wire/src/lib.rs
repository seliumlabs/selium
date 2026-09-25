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
pub use tables::{LiveTable, LiveTableMessage, LiveTableRecord, LiveTableView};

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

    /// Returns the current generation counter, or zero if unsupported.
    fn generation(&self) -> Result<u64>;

    /// Returns the shared region id for generation-wait registration, or 0
    /// if this transport is not backed by a shared memory region.
    fn region_id(&self) -> u64 {
        0
    }
}

/// Waits until the generation counter for `region_id` advances past
/// `observed_generation`, or parks the current task through the
/// generation-wait callback installed by the reactor.
///
/// Falls back to a waker wake if no callback is installed.
pub(crate) async fn generation_wait(region_id: u64, observed_generation: u64) {
    let mut yielded = false;
    std::future::poll_fn(move |cx| {
        if yielded {
            Poll::Ready(())
        } else {
            yielded = true;
            if !selium_memory::register_generation_wait(region_id, observed_generation, cx.waker())
            {
                cx.waker().wake_by_ref();
            }
            Poll::Pending
        }
    })
    .await
}
