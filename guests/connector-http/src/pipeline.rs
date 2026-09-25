//! Per-connection forwarding pipeline.
//!
//! Each accepted client connection runs a two-loop pipeline:
//!
//! - The **dispatch loop** owns the socket reader: it parses HTTP/1.1
//!   requests, resolves routes via discovery, establishes a typed session
//!   with the serving guest, and forwards each request with a
//!   connection-level correlation tag. Forwarded requests run concurrently
//!   up to a bounded window; when the window is full the loop stops reading
//!   from the socket — backpressure honesty at the edge.
//! - The **egress loop** owns the socket writer: it receives reply events,
//!   reorders them into request order via [`CorrelationMap`], and writes
//!   them to the wire (unary responses directly, streamed responses as
//!   chunked transfer encoding).
//!
//! Every queue in the pipeline is bounded, so the connector is never an
//! unbounded buffer.

// The forwarding seam is deliberately static-dispatch only (no dyn
// sessions), so the auto-trait caveat of async fn in traits does not
// apply here.
#![expect(async_fn_in_trait, reason = "seam is generic, never dyn-dispatched")]

use std::collections::{BTreeMap, VecDeque};

use futures::{StreamExt, stream::FuturesUnordered};
use selium_proto_http::{HttpRequest, HttpResponse, HttpStreamItem, HttpTrailer};
use selium_service::ResourceTarget;
use thiserror::Error;
use tokio::{
    io::{AsyncRead, AsyncWrite},
    sync::mpsc,
};
use tracing::warn;

use crate::{
    resolve::ResolverHandle,
    wire_out::{
        bad_gateway_response, internal_error_response, not_found_response,
        payload_too_large_response, write_chunk, write_response, write_stream_end,
        write_stream_head,
    },
};

/// Interface name registered by app guests that serve streamed responses.
///
/// Routes whose discovery registration carries this interface are forwarded
/// over server-streaming RPC; all other routes use unary RPC. Defined once
/// in the guest SDK (`selium_guest::net::http`) and shared with app guests.
pub use selium_guest::net::http::HTTP_STREAM_INTERFACE;

/// Default pipeline window: maximum concurrently in-flight forwarded
/// requests per connection.
pub const DEFAULT_MAX_PIPELINE: usize = 8;
/// Default capacity of the bounded reply-event channel.
pub const DEFAULT_REPLY_CAPACITY: usize = 16;

/// A typed session with a serving guest, able to forward one request.
pub trait ForwardSession {
    /// Forwards one typed request, emitting reply events on `out`.
    ///
    /// Implementations SHOULD emit exactly one terminal event; the
    /// pipeline guarantees termination regardless (see `pump`).
    ///
    /// `tag` is the connection-level correlation tag assigned to this
    /// request; every typed request on a connection carries a distinct one.
    async fn forward<S: ReplySink>(
        self,
        tag: u64,
        request: HttpRequest,
        out: &mut S,
    ) -> Result<(), ForwardError>;
}

/// Sink for typed reply events produced while forwarding one request.
pub trait ReplySink {
    /// Emits one reply event for the request being forwarded.
    async fn emit(&mut self, event: ReplyEvent) -> Result<(), ForwardError>;
}

/// Establishes typed sessions to resolved routes.
pub trait SessionFactory {
    /// The session type this factory produces.
    type Session: ForwardSession;

    /// Establishes a session with the serving guest for `target`.
    async fn connect(&self, target: &ResourceTarget) -> Result<Self::Session, ForwardError>;
}

/// Tunables for the per-connection pipeline.
#[derive(Debug, Clone, Copy)]
pub struct ConnectionConfig {
    /// Maximum concurrently in-flight forwarded requests per connection.
    /// When the window is full the connector pauses socket reads.
    pub max_pipeline: usize,
    /// Capacity of the bounded reply-event channel between the dispatch
    /// and egress loops.
    pub reply_capacity: usize,
}

/// A typed reply event produced while forwarding one request.
///
/// A forwarded request terminates with exactly one of [`Complete`](Self::Complete)
/// (unary reply), [`End`](Self::End) (stream finished), or [`Fail`](Self::Fail)
/// (no usable reply; the connector emits 502 unless a stream head was
/// already written).
#[derive(Debug, Clone, PartialEq)]
pub enum ReplyEvent {
    /// A complete unary response.
    Complete(HttpResponse),
    /// The head of a streamed response (status + headers; body follows as
    /// chunks and is written with chunked transfer encoding).
    Head(HttpResponse),
    /// A streamed body chunk.
    Chunk(Vec<u8>),
    /// A streamed trailer header.
    Trailer(HttpTrailer),
    /// End of a streamed response.
    End,
    /// Forwarding failed; emit a typed error response if nothing was
    /// written yet.
    Fail,
}

/// Errors surfaced by session establishment and forwarding.
#[derive(Debug, Error)]
pub enum ForwardError {
    /// Attaching to the route's host queue failed (stale route).
    #[error("attach: {0}")]
    Attach(String),
    /// Establishing the typed session failed.
    #[error("connect: {0}")]
    Connect(String),
    /// Sending the request or relaying reply events failed.
    #[error("send: {0}")]
    Send(String),
}

/// Correlation buffer that reorders reply events into request order.
///
/// Maps connection-level correlation tags (protocol-native request
/// ordering) to reply events: events for a later request are held back
/// until every earlier request has emitted its terminal event.
pub(crate) struct CorrelationMap {
    pending: BTreeMap<u64, VecDeque<ReplyEvent>>,
    next_to_send: u64,
}

/// Wraps a sink and records whether a terminal event (and whether a stream
/// head) has been emitted.
struct TerminalTracker<S> {
    inner: S,
    saw_terminal: bool,
    saw_head: bool,
}

/// Sink adapter that sends tagged events to the egress channel.
struct MpscSink {
    tag: u64,
    out: mpsc::Sender<(u64, ReplyEvent)>,
}

/// Production session factory: attaches the resolved route's host queue and
/// establishes a shared-memory typed RPC session.
///
/// Routes registered with [`HTTP_STREAM_INTERFACE`] get server-streaming
/// sessions (streamed bodies); all other routes get unary sessions.
#[derive(Clone, Copy, Default)]
pub struct ShmSessionFactory;

/// A production typed session with a serving guest.
pub enum ShmSession {
    /// Unary RPC: one `HttpRequest` in, one `HttpResponse` out.
    Unary(selium_shm::rpc::OwnedRpcClient<HttpRequest, HttpResponse>),
    /// Server-streaming RPC: one `HttpRequest` in, a stream of
    /// [`HttpStreamItem`]s out (head, chunks, trailers).
    Stream(selium_shm::rpc::OwnedServerStreamClient<HttpRequest, HttpStreamItem>),
}

impl Default for ConnectionConfig {
    fn default() -> Self {
        Self {
            max_pipeline: DEFAULT_MAX_PIPELINE,
            reply_capacity: DEFAULT_REPLY_CAPACITY,
        }
    }
}

impl ReplyEvent {
    /// Returns true if this event terminates its request's reply sequence.
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            ReplyEvent::Complete(_) | ReplyEvent::End | ReplyEvent::Fail
        )
    }
}

impl CorrelationMap {
    fn new() -> Self {
        Self {
            pending: BTreeMap::new(),
            next_to_send: 0,
        }
    }

    /// Inserts an event for `seq` and returns every event now flushable in
    /// request order (the contiguous prefix starting at the lowest
    /// not-yet-flushed sequence).
    fn insert_and_flush(&mut self, seq: u64, event: ReplyEvent) -> Vec<ReplyEvent> {
        self.pending.entry(seq).or_default().push_back(event);

        let mut ready = Vec::new();
        while let Some(queue) = self.pending.get_mut(&self.next_to_send) {
            while let Some(event) = queue.pop_front() {
                ready.push(event);
            }
            // Advance past the head sequence only once it has terminated;
            // an open stream keeps the head position so later sequences
            // stay buffered behind it.
            if ready.last().is_some_and(ReplyEvent::is_terminal) {
                self.pending.remove(&self.next_to_send);
                self.next_to_send += 1;
            } else {
                break;
            }
        }
        ready
    }
}

impl<S: ReplySink> ReplySink for TerminalTracker<S> {
    async fn emit(&mut self, event: ReplyEvent) -> Result<(), ForwardError> {
        match &event {
            ReplyEvent::Complete(_) | ReplyEvent::End | ReplyEvent::Fail => {
                self.saw_terminal = true;
            }
            ReplyEvent::Head(_) => self.saw_head = true,
            ReplyEvent::Chunk(_) | ReplyEvent::Trailer(_) => {}
        }
        self.inner.emit(event).await
    }
}

impl ReplySink for MpscSink {
    async fn emit(&mut self, event: ReplyEvent) -> Result<(), ForwardError> {
        match self.out.send((self.tag, event)).await {
            Ok(()) => Ok(()),
            Err(_) => Err(ForwardError::Send("egress loop closed".to_string())),
        }
    }
}

impl SessionFactory for ShmSessionFactory {
    type Session = ShmSession;

    async fn connect(&self, target: &ResourceTarget) -> Result<ShmSession, ForwardError> {
        let sender = selium_guest::ResourceSender::attach(target.resource_id)
            .map_err(|error| ForwardError::Attach(error.to_string()))?;

        let streamed = target
            .interface
            .as_ref()
            .is_some_and(|interface| interface.name == HTTP_STREAM_INTERFACE);

        if streamed {
            let client = selium_shm::rpc::connect_server_stream::<HttpRequest, HttpStreamItem, _>(
                sender, 0, 0,
            )
            .await
            .map_err(|error| ForwardError::Connect(error.to_string()))?;
            Ok(ShmSession::Stream(client))
        } else {
            let client = selium_shm::rpc::connect(sender, 0, 0)
                .await
                .map_err(|error| ForwardError::Connect(error.to_string()))?;
            Ok(ShmSession::Unary(client))
        }
    }
}

impl ForwardSession for ShmSession {
    async fn forward<S: ReplySink>(
        self,
        _tag: u64,
        request: HttpRequest,
        out: &mut S,
    ) -> Result<(), ForwardError> {
        match self {
            ShmSession::Unary(mut client) => {
                let response = client
                    .request(request)
                    .await
                    .map_err(|error| ForwardError::Send(error.to_string()))?;
                out.emit(ReplyEvent::Complete(response)).await
            }
            ShmSession::Stream(mut client) => {
                let mut stream = client
                    .call(request)
                    .await
                    .map_err(|error| ForwardError::Send(error.to_string()))?;
                while let Some(item) = stream.next().await {
                    let item = item.map_err(|error| ForwardError::Send(error.to_string()))?;
                    out.emit(stream_item_event(item)).await?;
                }
                Ok(())
            }
        }
    }
}

/// Runs the full pipeline for one client connection.
///
/// Generic over the socket type and the session factory so the protocol
/// logic is testable with in-memory streams and mock sessions.
pub async fn handle_connection<S, F>(
    stream: S,
    resolver: ResolverHandle,
    factory: F,
    config: ConnectionConfig,
) where
    S: AsyncRead + AsyncWrite + Unpin,
    F: SessionFactory,
{
    let (reader, writer) = tokio::io::split(stream);
    let (reply_tx, reply_rx) = mpsc::channel(config.reply_capacity);

    let dispatch = dispatch_loop(reader, resolver, factory, config, reply_tx);
    let egress = egress_loop(writer, reply_rx);
    let _ = tokio::join!(dispatch, egress);
}

/// Reads requests from the socket, resolves routes, and dispatches
/// forwards within the bounded pipeline window.
async fn dispatch_loop<R, F>(
    mut reader: R,
    resolver: ResolverHandle,
    factory: F,
    config: ConnectionConfig,
    reply_tx: mpsc::Sender<(u64, ReplyEvent)>,
) where
    R: AsyncRead + Unpin,
    F: SessionFactory,
{
    use crate::codec::{CodecError, HttpCodec, ReadResult, get_typed_header};

    let mut codec = HttpCodec::new();
    let mut next_seq: u64 = 0;
    let mut inflight = 0usize;
    let mut pumps = FuturesUnordered::new();

    loop {
        // Window full: pause socket reads until a forward completes. This
        // is the edge backpressure point — no request bytes are read while
        // the serving channels are saturated.
        if inflight >= config.max_pipeline {
            match pumps.next().await {
                Some(()) => inflight -= 1,
                None => break,
            }
            continue;
        }

        tokio::select! {
            biased;

            // Completed forwards free window slots; drain them eagerly so
            // the window tracks reality even while no socket data arrives.
            _done = pumps.next(), if inflight > 0 => {
                inflight -= 1;
            }

            read_result = codec.read_request(&mut reader) => {
                let request = match read_result {
                    Ok(ReadResult::Request(request)) => request,
                    Ok(ReadResult::Closed) => break,
                    Err(CodecError::RequestTooLarge) => {
                        // Reject with a typed 413, then close: the oversized
                        // body cannot be parsed or safely skipped, so the
                        // connection cannot continue.
                        let tag = next_seq;
                        next_seq += 1;
                        drop(reply_tx
                            .send((tag, ReplyEvent::Complete(payload_too_large_response())))
                            .await);
                        break;
                    }
                    Err(CodecError::PartialClosed) => break,
                    Err(CodecError::Io(error)) => {
                        warn!("http-connector: read error: {error}");
                        let tag = next_seq;
                        next_seq += 1;
                        drop(reply_tx
                            .send((tag, ReplyEvent::Complete(internal_error_response())))
                            .await);
                        break;
                    }
                };

                let tag = next_seq;
                next_seq += 1;

                let host = selium_abi::uri::normalize_host(
                    get_typed_header(&request.headers, "host").unwrap_or("localhost"),
                );

                let target = {
                    let mut resolver = resolver.lock().await;
                    match resolver.resolve(&host, &request.uri).await {
                        Ok(target) => target,
                        Err(_) => {
                            // No route: typed 404-equivalent without
                            // contacting any app guest.
                            drop(reply_tx
                                .send((tag, ReplyEvent::Complete(not_found_response())))
                                .await);
                            continue;
                        }
                    }
                };

                let session = match factory.connect(&target).await {
                    Ok(session) => session,
                    Err(error) => {
                        warn!(
                            "http-connector: session connect failed for {}: {error}",
                            target.uri
                        );
                        // Stale route: evict so the next request
                        // re-resolves via discovery.
                        resolver.lock().await.evict(&host, &request.uri);
                        drop(reply_tx
                            .send((tag, ReplyEvent::Complete(bad_gateway_response())))
                            .await);
                        continue;
                    }
                };

                inflight += 1;
                let out = reply_tx.clone();
                pumps.push(pump(session, tag, request, out));
            }
        }
    }

    // Connection ended: let in-flight forwards finish so their responses
    // still flush to the wire in order.
    let _ = next_seq;
    while pumps.next().await.is_some() {
        inflight = inflight.saturating_sub(1);
    }
    let _ = inflight;
}

/// Receives reply events and writes them to the wire in request order.
async fn egress_loop<W>(mut writer: W, mut reply_rx: mpsc::Receiver<(u64, ReplyEvent)>)
where
    W: AsyncWrite + Unpin,
{
    let mut correlation = CorrelationMap::new();
    let mut stream_open = false;
    let mut trailers: Vec<HttpTrailer> = Vec::new();

    while let Some((seq, event)) = reply_rx.recv().await {
        for event in correlation.insert_and_flush(seq, event) {
            let failed = match event {
                ReplyEvent::Complete(response) => {
                    stream_open = false;
                    trailers.clear();
                    write_response(&mut writer, &response).await.is_err()
                }
                ReplyEvent::Head(head) => {
                    stream_open = true;
                    write_stream_head(&mut writer, &head).await.is_err()
                }
                ReplyEvent::Chunk(data) => {
                    if data.is_empty() {
                        false
                    } else {
                        write_chunk(&mut writer, &data).await.is_err()
                    }
                }
                ReplyEvent::Trailer(trailer) => {
                    trailers.push(trailer);
                    false
                }
                ReplyEvent::End => {
                    let result = write_stream_end(&mut writer, &trailers).await.is_err();
                    trailers.clear();
                    stream_open = false;
                    result
                }
                ReplyEvent::Fail => {
                    if stream_open {
                        // A chunked response already started; it cannot be
                        // converted into an error status. Close instead.
                        warn!("http-connector: stream failed mid-response; closing connection");
                        return;
                    }
                    write_response(&mut writer, &bad_gateway_response())
                        .await
                        .is_err()
                }
            };
            if failed {
                warn!("http-connector: client write failed; closing connection");
                return;
            }
        }
    }
}

/// Forwards one request through its session, relaying reply events with
/// the connection-level tag and guaranteeing exactly one terminal event.
async fn pump<F: ForwardSession>(
    session: F,
    tag: u64,
    request: HttpRequest,
    out: mpsc::Sender<(u64, ReplyEvent)>,
) {
    let mut sink = TerminalTracker {
        inner: MpscSink { tag, out },
        saw_terminal: false,
        saw_head: false,
    };
    let result = session.forward(tag, request, &mut sink).await;
    if result.is_err() {
        warn!("http-connector: forward failed for tag {tag}");
    }
    // Guarantee termination even for misbehaving or interrupted sessions:
    // an open stream is ended, an unanswered request fails to 502.
    if !sink.saw_terminal {
        let event = if sink.saw_head {
            ReplyEvent::End
        } else {
            ReplyEvent::Fail
        };
        drop(sink.inner.emit(event).await);
    }
}

/// Maps a streamed response item to a reply event.
fn stream_item_event(item: HttpStreamItem) -> ReplyEvent {
    if item.is_head() {
        ReplyEvent::Head(HttpResponse::new(item.status, item.headers, Vec::new()))
    } else if item.is_chunk() {
        ReplyEvent::Chunk(item.data)
    } else if item.is_trailer() {
        ReplyEvent::Trailer(HttpTrailer::new(item.name, item.value))
    } else {
        ReplyEvent::Fail
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn complete(body: &str) -> ReplyEvent {
        ReplyEvent::Complete(HttpResponse::new(200, vec![], body.as_bytes().to_vec()))
    }

    #[test]
    fn correlation_in_order() {
        let mut map = CorrelationMap::new();

        let ready = map.insert_and_flush(0, complete("a"));
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0], complete("a"));

        let ready = map.insert_and_flush(1, complete("b"));
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0], complete("b"));
    }

    #[test]
    fn correlation_out_of_order_holds_back() {
        let mut map = CorrelationMap::new();

        // Response for seq 1 arrives first — held back behind seq 0.
        let ready = map.insert_and_flush(1, complete("1"));
        assert!(ready.is_empty(), "out-of-order reply must not flush");

        // Seq 0 arrives — both flush in request order.
        let ready = map.insert_and_flush(0, complete("0"));
        assert_eq!(ready, vec![complete("0"), complete("1")]);
    }

    #[test]
    fn correlation_gapped_sequence() {
        let mut map = CorrelationMap::new();

        let ready = map.insert_and_flush(2, complete("2"));
        assert!(ready.is_empty());

        // Seq 0 arrives but seq 1 is missing: only 0 flushes.
        let ready = map.insert_and_flush(0, complete("0"));
        assert_eq!(ready, vec![complete("0")]);

        let ready = map.insert_and_flush(1, complete("1"));
        assert_eq!(ready, vec![complete("1"), complete("2")]);
    }

    #[test]
    fn correlation_stream_holds_later_sequences_until_end() {
        let mut map = CorrelationMap::new();

        // Seq 0 opens a stream; seq 1 completes while it is open.
        let ready =
            map.insert_and_flush(0, ReplyEvent::Head(HttpResponse::new(200, vec![], vec![])));
        assert_eq!(ready.len(), 1);
        let ready = map.insert_and_flush(0, ReplyEvent::Chunk(b"x".to_vec()));
        assert_eq!(ready.len(), 1);
        let ready = map.insert_and_flush(1, complete("1"));
        assert!(ready.is_empty(), "seq 1 must wait for the open stream");

        // Stream ends: seq 1 flushes.
        let ready = map.insert_and_flush(0, ReplyEvent::End);
        assert_eq!(ready, vec![ReplyEvent::End, complete("1")]);
    }

    #[test]
    fn stream_item_mapping() {
        assert_eq!(
            stream_item_event(HttpStreamItem::head(200, vec![])),
            ReplyEvent::Head(HttpResponse::new(200, vec![], vec![]))
        );
        assert_eq!(
            stream_item_event(HttpStreamItem::chunk(b"data".to_vec())),
            ReplyEvent::Chunk(b"data".to_vec())
        );
        assert_eq!(
            stream_item_event(HttpStreamItem::trailer("x-t", "v")),
            ReplyEvent::Trailer(HttpTrailer::new("x-t".to_string(), "v".to_string()))
        );
        let unknown = HttpStreamItem {
            kind: 99,
            status: 0,
            headers: vec![],
            data: vec![],
            name: String::new(),
            value: String::new(),
        };
        assert_eq!(stream_item_event(unknown), ReplyEvent::Fail);
    }
}
