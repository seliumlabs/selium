//! Typed HTTP serve API for application guests.
//!
//! This module is the app-guest side of the HTTP connector: serve a named
//! route with discovery, accept typed RPC connections, and handle
//! `HttpRequest` → `HttpResponse` in a loop.
//!
//! A route is declared as a slash-separated service path under the guest's
//! own tenant (`"api"` or `"http/prod"`); the single `serve` declaration
//! derives the internal route (`sel://<tenant>/api`) and the wire names the
//! connector matches against the request Host (`api.<tenant>`, or
//! `api.<owned-domain>` when the tenant's domain is provisioned). The
//! request path resolves within the tenant's namespace relative to that
//! route, longest prefix first.
//!
//! ## Capability Model
//!
//! App guests using this API require **zero `Network` grants** — their
//! entire attack surface is channel attach. The HTTP connector terminates
//! TCP/TLS at the edge; plaintext crosses only capability-gated
//! shared-memory channels.
//!
//! The recommended grant is `ExplicitResource` scoped to the per-connection
//! channel region. Broad `UriPrefix` shared-memory grants widen exposure
//! and are documented as an anti-pattern for connector-served channels.
//!
//! ## Example
//!
//! ```ignore
//! use selium_guest::{net::http::HttpServe, entrypoint, Context};
//! use selium_proto_http::HttpResponse;
//!
//! #[entrypoint]
//! async fn my_app(mut ctx: Context) {
//!     // Serves `sel://<tenant>/api`; requests for `api.<tenant>/…` (or
//!     // `api.<owned-domain>/…`) route here.
//!     let mut serve = HttpServe::bind(&mut ctx, "api")
//!         .await
//!         .expect("bind failed");
//!
//!     while let Ok(mut conn) = serve.accept().await {
//!         while let Ok(req) = conn.recv().await {
//!             let typed = req.payload().unwrap();
//!             let response = HttpResponse::from_str(200, vec![], vec![]);
//!             req.reply(response).await.unwrap();
//!         }
//!     }
//! }
//! ```

use selium_abi::ResourceClass;
use selium_proto_http::{HttpHeader, HttpRequest, HttpResponse, HttpStreamItem};
use selium_service::{InterfaceMetadata, ResourceTarget};
use selium_shm::rpc::{self, RpcConnection, RpcError};
use thiserror::Error;

use crate::{Context, GuestError, ResourceListener, Serve};

/// Protocol handler name for HTTP routes. Used to pin serve listeners to the
/// HTTP connector's handoffs; not a URI scheme.
pub const HTTP_SCHEME: &str = "sel-http";
/// Interface marker for streamed HTTP serving.
///
/// [`HttpServeStream::bind`] registers this interface with discovery; the
/// connector routes matching requests through server-streaming RPC so
/// response bodies stream to the wire as chunked transfer encoding.
pub const HTTP_STREAM_INTERFACE: &str = "selium.http/stream";

/// A typed HTTP serve handle.
///
/// Wraps a `ResourceListener` and a discovery route registration for a named
/// service path. Each accepted connection is a typed `RpcConnection<HttpRequest, HttpResponse>`
/// that carries schema-encoded HTTP messages.
pub struct HttpServe {
    listener: ResourceListener,
    uri: String,
}

/// A single typed HTTP connection from the connector.
///
/// Wraps `RpcConnection<HttpRequest, HttpResponse>` for per-connection
/// channel hygiene. Each connection carries one request at a time with
/// tag-based correlation preserved end-to-end.
pub struct HttpConnection {
    conn: RpcConnection<HttpRequest, HttpResponse>,
}

/// A received HTTP request with the ability to reply.
pub struct HttpRequestHandle<'a> {
    req: selium_shm::rpc::RpcRequest<'a, HttpRequest, HttpResponse>,
}

/// Errors that can occur during typed HTTP serving.
#[derive(Debug, Error)]
pub enum HttpServeError {
    /// Failed to accept an incoming connection.
    #[error("accept error: {0}")]
    Accept(String),
    /// The remote connection was closed.
    #[error("connection closed")]
    ConnectionClosed,
    /// An RPC-level error occurred.
    #[error("RPC error: {0}")]
    Rpc(RpcError),
}

/// A typed HTTP serve handle for **streamed** responses.
///
/// Like [`HttpServe`], but registers [`HTTP_STREAM_INTERFACE`] with
/// discovery so the connector establishes server-streaming sessions. The
/// app guest produces a response head, then body chunks (and optional
/// trailers), which the connector writes to the wire incrementally with
/// chunked transfer encoding — the edge never buffers the whole body.
///
/// Same capability model as [`HttpServe`]: zero `Network` grants required.
///
/// ## Example
///
/// ```ignore
/// use selium_guest::{net::http::HttpServeStream, entrypoint, Context};
///
/// #[entrypoint]
/// async fn my_app(mut ctx: Context) {
///     // Serves `sel://<tenant>/events`; requests for `events.<tenant>/…`
///     // (or `events.<owned-domain>/…`) route here as streamed responses.
///     let mut serve = HttpServeStream::bind(&mut ctx, "events")
///         .await
///         .expect("bind failed");
///
///     while let Ok(mut conn) = serve.accept().await {
///         while let Ok(mut req) = conn.recv().await {
///             let _request = req.payload().unwrap();
///             req.send_head(200, vec![]).await.unwrap();
///             req.send_chunk(b"data: tick\n\n".to_vec()).await.unwrap();
///             req.finish().await.unwrap();
///         }
///     }
/// }
/// ```
pub struct HttpServeStream {
    listener: ResourceListener,
    uri: String,
}

/// A single streamed HTTP connection from the connector.
pub struct HttpStreamConnection {
    conn: rpc::ServerStreamConnection<HttpRequest, HttpStreamItem>,
}

/// A received HTTP request whose response is produced as a stream.
///
/// Response protocol: exactly one [`send_head`](Self::send_head) first,
/// then zero or more [`send_chunk`](Self::send_chunk) /
/// [`send_trailer`](Self::send_trailer) calls, then
/// [`finish`](Self::finish). The connector writes the head immediately
/// (chunked transfer encoding) and relays chunks to the wire as they are
/// produced — ring backpressure parks `send_chunk` when the client is
/// slow, so a slow consumer throttles the producer, not the edge buffer.
pub struct HttpStreamRequestHandle<'a> {
    req: rpc::ServerStreamRequest<'a, HttpRequest, HttpStreamItem>,
}

impl HttpServe {
    /// Serve a named route and register it with discovery.
    ///
    /// The `path` is a slash-separated service path under the guest's own
    /// tenant (`"api"` or `"http/prod"`); every segment must project to a
    /// DNS-safe wire label. The single declaration derives the internal
    /// route (`sel://<tenant>/api`) and the wire names the connector
    /// matches against the request Host (`api.<tenant>`, or
    /// `api.<owned-domain>` when the advisory domain table maps the
    /// tenant). The request path then resolves within the tenant's
    /// namespace relative to that route, longest prefix first. The guest
    /// creates its own listener; the runtime provisions nothing on its
    /// behalf.
    ///
    /// The guest requires a channel attach grant but **no `Network` grant** —
    /// networking is handled by the connector.
    ///
    /// The listener is **pinned to the registered `sel-http` protocol
    /// handler** (the HTTP connector): handoffs from any other process are
    /// refused, since handoff metadata is sender-controlled. Binding fails
    /// when no connector is registered.
    ///
    /// A guest running in the root/system tenant (an empty tenant label)
    /// additionally requires the system-registration capability to serve.
    pub async fn bind(ctx: &mut Context, path: &str) -> Result<Self, GuestError> {
        let mut listener = ResourceListener::create()
            .map_err(|e| GuestError::Host(format!("create listener: {e}")))?;
        super::pin_to_scheme_handler(&mut listener, HTTP_SCHEME)?;

        let target = http_target(&listener, None);
        let uri = ctx
            .serve(Serve {
                path: path_segments(path),
                target,
                default: false,
            })
            .await?;

        Ok(Self {
            listener,
            uri: uri.to_string(),
        })
    }

    /// Accept an incoming typed HTTP connection.
    ///
    /// Blocks until an incoming connection arrives from the connector,
    /// then builds a typed `RpcConnection<HttpRequest, HttpResponse>` over
    /// the shared-memory ring channel.
    pub async fn accept(&mut self) -> Result<HttpConnection, HttpServeError> {
        let incoming = self
            .listener
            .recv()
            .await
            .map_err(|e| HttpServeError::Accept(format!("recv: {e}")))?;

        let conn = rpc::accept::<HttpRequest, HttpResponse>(incoming.into())
            .map_err(HttpServeError::Rpc)?;

        Ok(HttpConnection { conn })
    }

    /// Returns the internal route URI this handle is bound to
    /// (`sel://<tenant>/<path>`).
    pub fn uri(&self) -> &str {
        &self.uri
    }
}

impl HttpConnection {
    /// Receive the next HTTP request on this connection.
    ///
    /// Returns an `HttpRequestHandle` that provides:
    /// - `payload()` / `into_payload()`: decode the typed `HttpRequest`
    /// - `reply(response)`: send a typed `HttpResponse` with correct
    ///   tag correlation
    pub async fn recv(&mut self) -> Result<HttpRequestHandle<'_>, HttpServeError> {
        self.conn
            .recv()
            .await
            .map(|req| HttpRequestHandle { req })
            .map_err(|e| match e {
                RpcError::ConnectionClosed => HttpServeError::ConnectionClosed,
                other => HttpServeError::Rpc(other),
            })
    }

    /// Returns the client process ID (the connector's process ID).
    pub fn client_process_id(&self) -> u64 {
        self.conn.client_process_id()
    }
}

impl HttpRequestHandle<'_> {
    /// Decode the typed `HttpRequest` payload.
    pub fn payload(&self) -> Result<HttpRequest, HttpServeError> {
        self.req.payload().map_err(HttpServeError::Rpc)
    }

    /// Decode and consume the typed `HttpRequest` payload.
    pub fn into_payload(self) -> Result<HttpRequest, HttpServeError> {
        self.req.into_payload().map_err(HttpServeError::Rpc)
    }

    /// Access the raw payload bytes.
    pub fn payload_bytes(&self) -> &[u8] {
        self.req.payload_bytes()
    }

    /// Send a typed `HttpResponse` back through the connector.
    ///
    /// The response carries the correct correlation tag so the connector
    /// can match it to the original request on the wire.
    pub async fn reply(self, response: HttpResponse) -> Result<(), HttpServeError> {
        self.req.reply(response).await.map_err(HttpServeError::Rpc)
    }
}

impl HttpServeStream {
    /// Serve a named route and register it with discovery as a streamed
    /// HTTP route: the target carries the [`HTTP_STREAM_INTERFACE`] marker,
    /// so the connector establishes server-streaming sessions for matching
    /// requests.
    ///
    /// The `path` is a slash-separated service path under the guest's own
    /// tenant; see [`HttpServe::bind`] for the derived names and the
    /// capability model.
    pub async fn bind(ctx: &mut Context, path: &str) -> Result<Self, GuestError> {
        let mut listener = ResourceListener::create()
            .map_err(|e| GuestError::Host(format!("create listener: {e}")))?;
        super::pin_to_scheme_handler(&mut listener, HTTP_SCHEME)?;

        let target = http_target(
            &listener,
            Some(InterfaceMetadata {
                name: HTTP_STREAM_INTERFACE.to_string(),
                methods: Vec::new(),
            }),
        );
        let uri = ctx
            .serve(Serve {
                path: path_segments(path),
                target,
                default: false,
            })
            .await?;

        Ok(Self {
            listener,
            uri: uri.to_string(),
        })
    }

    /// Accept an incoming streamed HTTP connection.
    pub async fn accept(&mut self) -> Result<HttpStreamConnection, HttpServeError> {
        let incoming = self
            .listener
            .recv()
            .await
            .map_err(|e| HttpServeError::Accept(format!("recv: {e}")))?;

        let conn = rpc::accept_server_stream::<HttpRequest, HttpStreamItem>(incoming.into())
            .map_err(HttpServeError::Rpc)?;

        Ok(HttpStreamConnection { conn })
    }

    /// Returns the internal route URI this handle is bound to
    /// (`sel://<tenant>/<path>`).
    pub fn uri(&self) -> &str {
        &self.uri
    }
}

impl HttpStreamConnection {
    /// Receive the next HTTP request on this connection.
    pub async fn recv(&mut self) -> Result<HttpStreamRequestHandle<'_>, HttpServeError> {
        self.conn
            .recv()
            .await
            .map(|req| HttpStreamRequestHandle { req })
            .map_err(|e| match e {
                RpcError::ConnectionClosed => HttpServeError::ConnectionClosed,
                other => HttpServeError::Rpc(other),
            })
    }

    /// Returns the client process ID (the connector's process ID).
    pub fn client_process_id(&self) -> u64 {
        self.conn.client_process_id()
    }
}

impl HttpStreamRequestHandle<'_> {
    /// Decode the typed `HttpRequest` payload.
    pub fn payload(&self) -> Result<HttpRequest, HttpServeError> {
        self.req.payload().map_err(HttpServeError::Rpc)
    }

    /// Decode and consume the typed `HttpRequest` payload.
    pub fn into_payload(self) -> Result<HttpRequest, HttpServeError> {
        self.req.into_payload().map_err(HttpServeError::Rpc)
    }

    /// Access the raw payload bytes.
    pub fn payload_bytes(&self) -> &[u8] {
        self.req.payload_bytes()
    }

    /// Send the response head (status + headers). Must be called exactly
    /// once before any chunks or trailers.
    pub async fn send_head(
        &mut self,
        status: u16,
        headers: Vec<HttpHeader>,
    ) -> Result<(), HttpServeError> {
        self.req
            .send_item(HttpStreamItem::head(status, headers))
            .await
            .map_err(HttpServeError::Rpc)
    }

    /// Send a body chunk to the client.
    pub async fn send_chunk(&mut self, data: Vec<u8>) -> Result<(), HttpServeError> {
        self.req
            .send_item(HttpStreamItem::chunk(data))
            .await
            .map_err(HttpServeError::Rpc)
    }

    /// Send a trailer header (written after the final chunk).
    pub async fn send_trailer(
        &mut self,
        name: impl Into<String>,
        value: impl Into<String>,
    ) -> Result<(), HttpServeError> {
        self.req
            .send_item(HttpStreamItem::trailer(name, value))
            .await
            .map_err(HttpServeError::Rpc)
    }

    /// Signal end-of-stream. The connector terminates the chunked body.
    pub async fn finish(&mut self) -> Result<(), HttpServeError> {
        self.req.finish().await.map_err(HttpServeError::Rpc)
    }

    /// Terminate the stream with an application error.
    pub async fn send_error(&mut self, message: impl Into<String>) -> Result<(), HttpServeError> {
        self.req
            .send_error(message)
            .await
            .map_err(HttpServeError::Rpc)
    }

    /// Check whether the client cancelled the stream (call between chunks).
    pub fn check_cancel(&mut self) -> bool {
        self.req.check_cancel()
    }
}

fn http_target(
    listener: &ResourceListener,
    interface: Option<InterfaceMetadata>,
) -> ResourceTarget {
    ResourceTarget {
        // Pinned by `serve` to the derived internal route URI.
        uri: String::new(),
        host_id: String::new(),
        resource_id: listener.descriptor().shared_id,
        interface,
        tenant: None,
        class: ResourceClass::HostQueue,
        labels: Vec::new(),
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

#[cfg(test)]
mod tests {
    #[test]
    fn bind_paths_split_into_route_segments() {
        assert_eq!(super::path_segments("api"), vec!["api".to_string()]);
        assert_eq!(super::path_segments("http/prod"), vec!["http", "prod"]);
        assert!(super::path_segments("").is_empty());
        assert!(super::path_segments("/").is_empty());
    }
}
