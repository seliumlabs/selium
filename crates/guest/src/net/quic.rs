//! Byte-transport QUIC serve API for application guests.
//!
//! This module is the app-guest side of the QUIC connector: serve a named
//! route with discovery and accept per-stream byte channels from the
//! connector, then frame the bytes with any user schema.
//!
//! A route is declared as a slash-separated service path under the guest's
//! own tenant (`"my-app"` or `"http/prod"`); the single `serve` declaration
//! derives both the internal path (`sel://<tenant>/my-app`) and the wire
//! name the connector matches against the QUIC handshake's SNI
//! (`my-app.<tenant>`, or `my-app.<owned-domain>` when the tenant's domain
//! is provisioned).
//!
//! ## Capability model
//!
//! App guests served by the QUIC connector require **zero `Network` grants**
//! and **zero quinn dependency**. QUIC is terminated at the edge by the
//! connector, and only capability-gated shared-memory byte channels reach the
//! app guest. The entire attack surface is channel attach.
//!
//! The recommended grant is [`ExplicitResource`](selium_abi::CapabilityGrant)
//! scoped to each per-stream channel region. Broad shared-memory `UriPrefix`
//! grants widen exposure to *every* connector-served channel and are
//! documented here as an anti-pattern: each stream's channel SHOULD carry its
//! own `ExplicitResource` grant so streams on one connection cannot attach to
//! another stream's region (see `selium-runtime`'s `quic_connector` substrate
//! tests).
//!
//! [`ExplicitResource`]: selium_abi::ResourceSelector::ExplicitResource
//!
//! ## Example
//!
//! ```ignore
//! use selium_guest::{net::quic::QuicServe, entrypoint, Context};
//! use tokio::io::{AsyncReadExt, AsyncWriteExt};
//!
//! #[entrypoint]
//! async fn my_app(mut ctx: Context) {
//!     // Serves `sel://<tenant>/my-app`; clients connect with SNI
//!     // `my-app.<tenant>` (or `my-app.<owned-domain>`).
//!     let mut serve = QuicServe::bind(&mut ctx, "my-app")
//!         .await
//!         .expect("bind failed");
//!
//!     while let Ok(mut stream) = serve.accept().await {
//!         let mut buf = vec![0u8; 1024];
//!         let n = stream.read(&mut buf).await.expect("read");
//!         stream.write_all(&buf[..n]).await.expect("echo");
//!         drop(stream);
//!     }
//! }
//! ```

use std::{
    pin::Pin,
    task::{Context as TaskContext, Poll},
};

use super::bytes::ByteStream;
use selium_abi::ResourceClass;
use selium_service::{InterfaceMetadata, ResourceTarget};
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

use crate::{Context, GuestError, ResourceListener, Serve};

/// Protocol handler name for QUIC routes. Used to pin serve listeners to the
/// QUIC connector's handoffs; not a URI scheme.
pub const QUIC_SCHEME: &str = "sel-quic";
/// Interface marker registered by app guests that serve QUIC byte channels.
pub const QUIC_STREAM_INTERFACE: &str = "selium.quic/stream";

/// A byte-transport QUIC serve handle.
///
/// Wraps a [`ResourceListener`] and a discovery route registration for a
/// named service path (`my-app` under the guest's tenant). Each accepted
/// stream is a [`QuicStream`] byte channel from the connector.
pub struct QuicServe {
    listener: ResourceListener,
    uri: String,
}

/// A single per-stream byte channel from the QUIC connector.
///
/// Presents the relayed stream as `AsyncRead` + `AsyncWrite`: bytes read are
/// the external client's bytes (in order), and bytes written are relayed back
/// to the client. Zero `Network` grants are required — only the channel attach
/// grant for this stream's region.
pub struct QuicStream {
    inner: ByteStream,
}

/// Errors that can occur while serving QUIC byte channels.
#[derive(Debug, Error)]
pub enum QuicServeError {
    /// Failed to accept an incoming stream.
    #[error("accept: {0}")]
    Accept(String),
    /// The remote (connector) closed the listener.
    #[error("listener closed")]
    Closed,
}

impl QuicServe {
    /// Serves a named route and registers it with discovery.
    ///
    /// The `path` is a slash-separated service path under the guest's own
    /// tenant (`"my-app"` or `"http/prod"`); every segment must project to a
    /// DNS-safe wire label. The single declaration derives the internal
    /// route (`sel://<tenant>/my-app`) and the wire name the connector
    /// matches against the connection's SNI (`my-app.<tenant>`, or
    /// `my-app.<owned-domain>` when the advisory domain table maps the
    /// tenant). The guest creates its own listener; the runtime provisions
    /// nothing on its behalf.
    ///
    /// The listener is **pinned to the registered `sel-quic` protocol
    /// handler** (the QUIC connector): handoffs from any other process are
    /// refused. Handoff metadata is sender-controlled, so without the pin
    /// any guest able to attach the queue could forge handoffs. Binding
    /// fails when no connector is registered — serving unpinned handoffs
    /// would reintroduce that vector.
    ///
    /// The guest requires a channel attach grant but **no `Network` grant** —
    /// QUIC is terminated and relayed by the connector.
    ///
    /// A guest running in the root/system tenant (an empty tenant label)
    /// additionally requires the system-registration capability to serve.
    pub async fn bind(ctx: &mut Context, path: &str) -> Result<Self, GuestError> {
        let mut listener = ResourceListener::create()
            .map_err(|e| GuestError::Host(format!("create listener: {e}")))?;
        super::pin_to_scheme_handler(&mut listener, QUIC_SCHEME)?;

        let target = quic_target(&listener);
        let uri = ctx
            .serve(Serve {
                path: path_segments(path),
                target,
                default: false,
            })
            .await?;

        Ok(Self { listener, uri })
    }

    /// Accepts the next delivered stream region from the connector.
    ///
    /// Attaches the delivered two-ring region as a [`QuicStream`] byte
    /// channel. The connector delivers one region per accepted bidirectional
    /// QUIC stream.
    pub async fn accept(&mut self) -> Result<QuicStream, QuicServeError> {
        let incoming = self
            .listener
            .recv()
            .await
            .map_err(|e| QuicServeError::Accept(format!("recv: {e}")))?;

        let stream = ByteStream::attach_blocking(incoming.shared_id)
            .map_err(|e| QuicServeError::Accept(format!("attach stream: {e}")))?;

        Ok(QuicStream { inner: stream })
    }

    /// Returns the internal route URI this handle is bound to
    /// (`sel://<tenant>/<path>`).
    pub fn uri(&self) -> &str {
        &self.uri
    }
}

impl QuicStream {
    /// Builds a `QuicStream` from a delivered region's shared id.
    ///
    /// Separate from [`ByteStream::attach`] because the blocking writer is
    /// required for peer-to-peer close semantics (the connector observes EOF
    /// when this stream drops its write half).
    pub fn from_shared_id(shared_id: u64) -> Result<Self, GuestError> {
        Ok(Self {
            inner: ByteStream::attach_blocking(shared_id)?,
        })
    }
}

impl AsyncRead for QuicStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_read(cx, buf)
    }
}

impl AsyncWrite for QuicStream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.get_mut().inner).poll_write(cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
    }
}

/// Splits a slash-separated service path into route segments, ignoring
/// empty segments. The path must project to DNS-safe wire labels —
/// [`Context::serve`](crate::Context::serve) rejects typed resource paths
/// (`region/7`) and non-DNS-safe segments up front.
fn path_segments(path: &str) -> Vec<String> {
    path.split('/')
        .filter(|segment| !segment.is_empty())
        .map(str::to_string)
        .collect()
}

fn quic_target(listener: &ResourceListener) -> ResourceTarget {
    ResourceTarget {
        // Pinned by `serve` to the derived internal route URI.
        uri: String::new(),
        host_id: String::new(),
        resource_id: listener.descriptor().shared_id,
        interface: Some(InterfaceMetadata {
            name: QUIC_STREAM_INTERFACE.to_string(),
            methods: Vec::new(),
        }),
        tenant: None,
        class: ResourceClass::HostQueue,
        labels: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use selium_shm::byte_channel;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn setup() {
        drop(selium_memory::set_region_provider(Box::new(
            selium_memory::HeapRegionProvider::new(),
        )));
    }

    #[test]
    fn bind_paths_split_into_route_segments() {
        assert_eq!(path_segments("my-app"), vec!["my-app".to_string()]);
        assert_eq!(path_segments("http/prod"), vec!["http", "prod"]);
        // Empty and slash-only paths carry no segments; `serve` rejects them
        // (a named service must project to a wire name).
        assert!(path_segments("").is_empty());
        assert!(path_segments("/").is_empty());
    }

    #[test]
    fn bind_paths_must_project_to_wire_names() {
        // Typed resource paths and non-DNS-safe segments never project;
        // `serve` rejects them before reaching discovery.
        assert!(selium_abi::uri::labels_from_path("region/7").is_none());
        assert!(selium_abi::uri::labels_from_path("My_App").is_none());
        // A served path projects to the wire name the connector matches
        // against SNI: `["http", "prod"]` under tenant `acme` →
        // `prod.http.acme`.
        assert_eq!(
            selium_abi::uri::labels_from_path("my-app"),
            Some(vec!["my-app".to_string()])
        );
    }

    #[tokio::test]
    async fn quic_stream_round_trips_bytes_with_connector_peer() {
        setup();

        // Allocate a region pair the way the connector would, then attach the
        // app-guest half as a QuicStream.
        let (ring_to_guest, ring_from_guest, shared_id, _region) =
            byte_channel::create(4096, 4096).expect("create");
        let region = selium_memory::region_provider()
            .expect("provider")
            .attach(shared_id, None, selium_abi::RegionProt::ReadWrite)
            .expect("attach");

        // The connector reads the guest's outbound ring and writes the
        // guest's inbound ring (the mirror half).
        let mut peer =
            ByteStream::from_ring_channels(&ring_from_guest, &ring_to_guest, region, true)
                .expect("peer");
        let mut stream = QuicStream::from_shared_id(shared_id).expect("quic stream");

        peer.write_all(b"request").await.expect("peer write");
        let mut buf = [0u8; 7];
        stream.read_exact(&mut buf).await.expect("guest read");
        assert_eq!(&buf, b"request");

        stream.write_all(b"response").await.expect("guest write");
        let mut buf = [0u8; 8];
        peer.read_exact(&mut buf).await.expect("peer read");
        assert_eq!(&buf, b"response");
    }
}
