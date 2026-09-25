//! Per-stream bridge channel system guest.
//!
//! A bridge channel bridges exactly one external QUIC stream to exactly one
//! served target, dispatching on the resolved target's class:
//!
//! - **channel targets** are spliced transparently: `selium-wire` frames
//!   pass unchanged between the relayed byte stream and the fabric ring
//!   (pub/sub, live tables) — correlation tags, flags, and payload bytes
//!   are preserved verbatim;
//! - **host-queue targets** (RPC serving listeners) are rendezvoused: the
//!   pipe allocates a two-ring session region on the external client's
//!   behalf (a remote QUIC client cannot make hostcalls), enqueues the
//!   session id into the served queue, and relays frames between the
//!   stream and the session rings — the same session shape the serving
//!   guest's `rpc::accept` attaches for internal clients.
//!
//! In both modes the pipe never inspects application payloads. Closing either
//! end tears down the whole pipe: a client FIN drops the fabric membership
//! (or frees the session region, so the serving guest observes session end)
//! and exits; a fabric/session close finishes the client's stream and exits.
//!
//! This crate is a deployable guest, but its core ([`bridge_pipe`]) is
//! dependency-injected (the target resolver and the queue enqueue) and
//! exercised natively by unit tests with the heap region provider, mirroring
//! the connector's `stream.rs`.

use std::{
    pin::Pin,
    task::{Context as TaskContext, Poll},
};

use anyhow::Context as _;
use selium_abi::ResourceClass;
use selium_guest::{
    Context, GuestError, ResourceSender, Result, entrypoint, info, mark_ready,
    net::{
        ByteStream,
        bytes::{ByteStreamReader, ByteStreamWriter},
    },
};
use selium_service::{FlatMsg, ResourceTarget};
use selium_shm::{
    Channel, byte_channel,
    channels::{BlockingReader, BlockingWriter},
    free_region,
};
use selium_wire::{
    MessageTransport,
    error::Error as WireError,
    framed::{FramedRead, FramedWrite},
};

// The handshake/termination contract is now owned by `selium-wire`; the bridge
// channel re-exports it so the shared type and its termination codes remain
// available from this crate with no wire change.
pub use selium_wire::{PipeControl, TERMINATE_ATTACH_FAILED, TERMINATE_BAD_HANDSHAKE};

const OWN_RING_READERS: u64 = 1;
/// The pipe's own contribution to the fabric ring's member counts: its
/// single counting writer (the write adapter's) and single blocking reader
/// (the read adapter's). "All inner peers gone" is observable as
/// `writer_count == OWN_RING_WRITERS && reader_count == OWN_RING_READERS`.
///
/// Count-based liveness cannot distinguish "every inner peer left" from "no
/// inner peer ever attached": a pipe bridging a memberless fabric finishes
/// the client's stream (a clean FIN the client can retry) rather than
/// parking forever. Non-blocking reader-only inner peers are invisible to
/// the reader count; writers and blocking readers — the norms for fabric
/// members — are both counted.
const OWN_RING_WRITERS: u64 = 1;
/// Accept signal on a session's request ring: the pipe's own writer plus
/// the serving guest's request-transport writer. Observing it guarantees
/// the server's request reader is already registered (a transport registers
/// its eager reader before its writer), so no relayed frame can fall behind
/// the reader's start position.
const SESSION_ACCEPT_WRITERS: u64 = 2;
/// Session ring capacity for a rendezvoused RPC session, per direction.
///
/// A frame larger than the ring can never be relayed (Park backpressure
/// parks the writer forever), so this bounds single request/reply frames
/// for external RPC clients. It is deliberately sized above the internal
/// `rpc::connect` defaults so module uploads admit through the control
/// surface; the two rings are per external stream and freed on teardown.
const SESSION_RING_CAPACITY: u64 = 1 << 20;
/// The serving guest's writers on a session's reply ring: the pipe holds
/// none, so the count reaching zero after accept means the server's side
/// of the session has ended.
const SESSION_SERVER_WRITERS: u64 = 1;

/// A [`MessageTransport`] adapting the read half of the relayed byte stream.
///
/// [`FramedRead`] only exercises the read side, so the write side is stubbed.
struct StreamReadTransport {
    reader: ByteStreamReader,
}

/// A [`MessageTransport`] adapting the write half of the relayed byte stream.
///
/// [`FramedWrite`] only exercises the write side, so the read side is stubbed.
struct StreamWriteTransport {
    writer: ByteStreamWriter,
}

/// A read-only [`MessageTransport`] over the fabric ring's blocking reader.
///
/// Holds **no writer** on the ring: the pipe's single counting writer lives
/// in [`RingWriteTransport`], so the pipe's `writer_count` contribution is
/// exactly one and "all inner writers gone" stays observable. The write
/// side is stubbed — [`FramedRead`] only exercises the read side.
struct RingReadTransport {
    reader: BlockingReader,
}

/// A write-only [`MessageTransport`] over the fabric ring's blocking writer.
///
/// This is the pipe's **only counting writer** on the fabric ring: inner
/// guests observe the pipe's membership (and its death, as a `writer_count`
/// drop) through it. The read side is stubbed — [`FramedWrite`] only
/// exercises the write side.
struct RingWriteTransport {
    writer: BlockingWriter,
}

/// Yields once and re-checks, keeping the reactor alive while the
/// (concurrent) serving guest completes its accept.
///
/// Mirrors `rpc::connect`'s accept wait: a transport registration on the
/// far side advances no generation counter, so there is nothing to park
/// on — the condition is re-checked in a self-waking spin until the
/// serving guest's writer lands. Identical to an internal client's
/// behaviour for a route that never accepts.
struct YieldOnce(bool);

impl tokio::io::AsyncRead for StreamReadTransport {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.reader).poll_read(cx, buf)
    }
}

impl tokio::io::AsyncWrite for StreamReadTransport {
    fn poll_write(
        self: Pin<&mut Self>,
        _cx: &mut TaskContext<'_>,
        _buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Poll::Ready(Err(std::io::ErrorKind::Unsupported.into()))
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut TaskContext<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut TaskContext<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

impl MessageTransport for StreamReadTransport {
    type Error = std::io::Error;

    fn poll_ready(
        self: Pin<&mut Self>,
        _cx: &mut TaskContext<'_>,
    ) -> Poll<selium_wire::Result<bool>> {
        Poll::Ready(Ok(true))
    }

    fn poll_peer_closed(
        self: Pin<&mut Self>,
        _cx: &mut TaskContext<'_>,
    ) -> Poll<selium_wire::Result<bool>> {
        Poll::Ready(Ok(false))
    }

    fn generation(&self) -> selium_wire::Result<u64> {
        Ok(0)
    }
}

impl tokio::io::AsyncRead for StreamWriteTransport {
    fn poll_read(
        self: Pin<&mut Self>,
        _cx: &mut TaskContext<'_>,
        _buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Poll::Ready(Err(std::io::ErrorKind::Unsupported.into()))
    }
}

impl tokio::io::AsyncWrite for StreamWriteTransport {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.writer).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.writer).poll_flush(cx)
    }

    fn poll_shutdown(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.writer).poll_shutdown(cx)
    }
}

impl MessageTransport for StreamWriteTransport {
    type Error = std::io::Error;

    fn poll_ready(
        self: Pin<&mut Self>,
        _cx: &mut TaskContext<'_>,
    ) -> Poll<selium_wire::Result<bool>> {
        Poll::Ready(Ok(true))
    }

    fn poll_peer_closed(
        self: Pin<&mut Self>,
        _cx: &mut TaskContext<'_>,
    ) -> Poll<selium_wire::Result<bool>> {
        Poll::Ready(Ok(false))
    }

    fn generation(&self) -> selium_wire::Result<u64> {
        Ok(0)
    }
}

impl tokio::io::AsyncRead for RingReadTransport {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.reader).poll_read(cx, buf)
    }
}

impl tokio::io::AsyncWrite for RingReadTransport {
    fn poll_write(
        self: Pin<&mut Self>,
        _cx: &mut TaskContext<'_>,
        _buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Poll::Ready(Err(std::io::ErrorKind::Unsupported.into()))
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut TaskContext<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut TaskContext<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

impl MessageTransport for RingReadTransport {
    type Error = std::io::Error;

    fn poll_ready(
        self: Pin<&mut Self>,
        _cx: &mut TaskContext<'_>,
    ) -> Poll<selium_wire::Result<bool>> {
        Poll::Ready(Ok(true))
    }

    fn poll_peer_closed(
        self: Pin<&mut Self>,
        _cx: &mut TaskContext<'_>,
    ) -> Poll<selium_wire::Result<bool>> {
        Poll::Ready(Ok(false))
    }

    fn generation(&self) -> selium_wire::Result<u64> {
        Ok(0)
    }
}

impl tokio::io::AsyncRead for RingWriteTransport {
    fn poll_read(
        self: Pin<&mut Self>,
        _cx: &mut TaskContext<'_>,
        _buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Poll::Ready(Err(std::io::ErrorKind::Unsupported.into()))
    }
}

impl tokio::io::AsyncWrite for RingWriteTransport {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.writer).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.writer).poll_flush(cx)
    }

    fn poll_shutdown(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.writer).poll_shutdown(cx)
    }
}

impl MessageTransport for RingWriteTransport {
    type Error = std::io::Error;

    fn poll_ready(
        self: Pin<&mut Self>,
        _cx: &mut TaskContext<'_>,
    ) -> Poll<selium_wire::Result<bool>> {
        Poll::Ready(Ok(true))
    }

    fn poll_peer_closed(
        self: Pin<&mut Self>,
        _cx: &mut TaskContext<'_>,
    ) -> Poll<selium_wire::Result<bool>> {
        Poll::Ready(Ok(false))
    }

    fn generation(&self) -> selium_wire::Result<u64> {
        Ok(0)
    }
}

impl std::future::Future for YieldOnce {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<()> {
        if self.0 {
            Poll::Ready(())
        } else {
            self.0 = true;
            cx.waker().wake_by_ref();
            Poll::Pending
        }
    }
}

/// The bridge pipe core: handshake → resolve → dispatch (splice or
/// rendezvous) → accepted reply → relay → teardown.
///
/// `resolve` maps a target URI to its served target (the discovery lookup
/// in production); the target's class selects the pipe mode:
///
/// - channel targets: a transparent splice into the channel ring — frames
///   pass through unchanged (pub/sub, live tables);
/// - host-queue targets: an RPC rendezvous — the pipe establishes a
///   session region on the external client's behalf and enqueues it into
///   the served queue (`enqueue` performs the host-queue handoff the
///   internal `rpc::connect` performs client-side), because a remote QUIC
///   client cannot make hostcalls.
///
/// Both closures stay generic for the native test seam; the entrypoint
/// supplies the discovery-backed resolver and the `ResourceSender` enqueue.
pub async fn bridge_pipe<Resolve, Fut, Enqueue, EnqFut>(
    stream: ByteStream,
    resolve: Resolve,
    enqueue: Enqueue,
) where
    Resolve: FnOnce(String) -> Fut,
    Fut: std::future::Future<Output = Result<ResourceTarget>>,
    Enqueue: FnOnce(u64, u64) -> EnqFut,
    EnqFut: std::future::Future<Output = Result<()>>,
{
    let (reader, writer) = stream.split();
    let mut stream_read = FramedRead::new(StreamReadTransport { reader });
    let mut stream_write = FramedWrite::new(StreamWriteTransport { writer });

    // 1. Typed pipe handshake: the first stream frame names the target URI.
    let Some((handshake_payload, _, _)) = next_frame(&mut stream_read).await else {
        // Client closed before the handshake; nothing more to do.
        return;
    };
    let uri = match FlatMsg::decode(&handshake_payload) {
        Ok(PipeControl::Handshake { uri }) => uri,
        _ => {
            terminate(&mut stream_write, TERMINATE_BAD_HANDSHAKE).await;
            return;
        }
    };
    selium_guest::info!("bridge-channel: handshake for {uri}");

    // 2. Resolve the served target. Resolution is tenant-scoped by the
    //    bridge channel's grants.
    let target = match resolve(uri).await {
        Ok(target) => target,
        Err(_) => {
            terminate(&mut stream_write, TERMINATE_ATTACH_FAILED).await;
            return;
        }
    };
    selium_guest::info!(
        "bridge-channel: resolved target class={:?} id={}",
        target.class,
        target.resource_id
    );

    // 3. Dispatch on the target's class: host-queue serving listeners take
    //    the RPC rendezvous; everything else keeps the transparent splice.
    match target.class {
        ResourceClass::HostQueue => {
            rendezvous_pipe(stream_read, stream_write, move |session_id| {
                enqueue(target.resource_id, session_id)
            })
            .await;
        }
        _ => splice_pipe(stream_read, stream_write, target.resource_id).await,
    }
}

/// Bridge channel entrypoint.
///
/// Arguments: the bootstrap discovery `Context` (built by the entrypoint
/// macro, used for target-URI resolution) and the relayed byte-channel
/// region `shared_id` (delivered by `bridge-server`).
#[entrypoint]
async fn bridge_channel(mut ctx: Context, shared_id: u64) -> anyhow::Result<()> {
    drop(selium_guest::log::init());
    info!("bridge-channel: started");

    let stream = ByteStream::attach_blocking(shared_id)
        .with_context(|| "bridge-channel: attach stream region failed")?;

    mark_ready();

    bridge_pipe(
        stream,
        move |uri| async move {
            let target = ctx
                .lookup(&uri)
                .await?
                .ok_or_else(|| GuestError::Host(format!("target not found: {uri}")))?;
            Ok(target)
        },
        // The queue handoff for a rendezvoused session. The handoff metadata
        // is empty today: the resolved client identity cannot yet be
        // forwarded to a guest-spawned child (guest `Process::start` carries
        // integer arguments only), and serving guests authorize via the
        // runtime's capability enforcement, not the metadata. Identity
        // pass-through lands with pointer-argument spawn support.
        |queue_id, session_id| async move {
            let sender = ResourceSender::attach(queue_id)?;
            sender.send_with_metadata(session_id, Vec::new()).await
        },
    )
    .await;

    Ok(())
}

/// Reads the next complete frame, parking on the transport's read waker
/// until one arrives.
///
/// The relayed stream and the session rings are shared with other
/// processes, and a `yield_now` spin parks the task until the next
/// host-driven reactor entry — which never arrives for a pure
/// shared-memory wait. `read_frame_async` instead parks through the
/// generation-wait mechanism, so a peer's write (or close) re-polls this
/// guest through the runtime's wake path. Any read failure (peer EOF or
/// transport error) ends the frame stream.
async fn next_frame<M: MessageTransport>(reader: &mut FramedRead<M>) -> Option<(Vec<u8>, u32, u8)> {
    reader.read_frame_async().await.ok()
}

/// Copies frames from the fabric ring to the relayed stream (fabric → client).
///
/// The fabric half ends when the ring read errors, or when the ring quiesces:
/// no data is pending and only the pipe's own members remain (every inner
/// writer and blocking reader is gone). `fabric` is moved in so the quiesce
/// check can read the live member counts.
async fn pump_ring_to_stream(
    mut ring_read: FramedRead<RingReadTransport>,
    mut stream_write: FramedWrite<StreamWriteTransport>,
    fabric: Channel,
) {
    // Fabric close ends the loop; dropping `stream_write` finishes the client
    // stream.
    loop {
        match ring_read.read_frame() {
            Ok((payload, tag, flags)) => {
                if stream_write
                    .write_frame_with_flags_async(&payload, tag, flags)
                    .await
                    .is_err()
                {
                    break;
                }
            }
            Err(WireError::BufferEmpty) => {
                // No data pending. If only the pipe's own members remain,
                // the fabric is closed: every inner writer and blocking
                // reader is gone, so nothing further can ever arrive.
                // Finish the client's stream instead of parking in
                // BufferEmpty forever.
                let writers = fabric.ring().region().load_writer_count().unwrap_or(0);
                let readers = fabric.ring().region().read_reader_count().unwrap_or(0);
                if writers <= OWN_RING_WRITERS && readers <= OWN_RING_READERS {
                    break;
                }
                // An inner peer is still attached: park until the ring
                // advances (its next write — or its close, which also
                // bumps the generation — re-polls the quiesce check).
                wait_for_ring_advance(fabric.ring().region()).await;
            }
            Err(_) => break,
        }
    }
}

/// Relays frames reply ring → stream (server → client), gated on the
/// serving guest's accept and ending when the server's side of the session
/// closes.
async fn pump_session_to_stream(
    mut session_read: FramedRead<RingReadTransport>,
    mut stream_write: FramedWrite<StreamWriteTransport>,
    reply_channel: Channel,
) {
    // Accept gate on the reply ring: wait until the server's reply
    // transport writer has joined. Observing it guarantees the server's
    // reply reader is already registered; afterwards the reader's own
    // EOF semantics apply (before the gate passes, an empty reply ring
    // means "not accepted yet", not "session ended" — the pipe holds no
    // reply writer, so the ring's writer count is zero until accept).
    loop {
        match reply_channel.ring().region().load_writer_count() {
            Ok(count) if count >= SESSION_SERVER_WRITERS => break,
            Ok(_) => YieldOnce(false).await,
            Err(_) => return,
        }
    }
    // Session end (all server writers gone → ring EOF) finishes the
    // client's stream; a failing stream write ends the relay the other
    // way. Both halves park through the generation-wait mechanism, so a
    // serving guest in another process drives the relay.
    while let Ok((payload, tag, flags)) = session_read.read_frame_async().await {
        if stream_write
            .write_frame_with_flags_async(&payload, tag, flags)
            .await
            .is_err()
        {
            break;
        }
    }
}

/// Copies frames from the relayed stream to the fabric ring (client → fabric).
async fn pump_stream_to_ring(
    mut stream_read: FramedRead<StreamReadTransport>,
    mut ring_write: FramedWrite<RingWriteTransport>,
) {
    // Client FIN (stream EOF) ends the loop; dropping `ring_write` releases
    // the fabric membership.
    while let Some((payload, tag, flags)) = next_frame(&mut stream_read).await {
        if ring_write
            .write_frame_with_flags_async(&payload, tag, flags)
            .await
            .is_err()
        {
            break;
        }
    }
}

/// Relays frames stream → request ring (client → server), gated on the
/// serving guest's accept.
///
/// The gate mirrors `rpc::connect`'s accept wait: a blocking reader starts
/// at the ring tail at registration, so a frame written before the server
/// registers its reader is invisible to it forever. Waiting for the second
/// writer makes first-frame ordering deterministic. A serving guest that
/// never accepts parks the pipe here — the honest backpressure outcome for
/// a route nobody serves, identical to an internal client.
async fn pump_stream_to_session(
    mut stream_read: FramedRead<StreamReadTransport>,
    mut session_write: FramedWrite<RingWriteTransport>,
    request_channel: Channel,
) {
    loop {
        match request_channel.ring().region().load_writer_count() {
            Ok(count) if count >= SESSION_ACCEPT_WRITERS => break,
            Ok(_) => YieldOnce(false).await,
            Err(_) => return,
        }
    }
    // Client FIN (stream EOF) ends the loop; dropping `session_write`
    // releases the request-ring membership.
    while let Some((payload, tag, flags)) = next_frame(&mut stream_read).await {
        if session_write
            .write_frame_with_flags_async(&payload, tag, flags)
            .await
            .is_err()
        {
            break;
        }
    }
}

/// RPC rendezvous into a served host queue: the pipe plays the internal
/// [`rpc::connect`](selium_shm::rpc) client role on the external client's
/// behalf.
///
/// Allocates a two-ring session region (request ring client → server, reply
/// ring server → client — the layout the serving guest's `rpc::accept`
/// attaches), enqueues the session id into the served queue via `enqueue`,
/// replies accepted, and relays frames between the stream and the session
/// rings without decoding them. Teardown frees the session region (the pipe
/// is its allocator), so the serving guest observes session end —
/// mirroring an internal client's drop.
async fn rendezvous_pipe<Enqueue, EnqFut>(
    stream_read: FramedRead<StreamReadTransport>,
    mut stream_write: FramedWrite<StreamWriteTransport>,
    enqueue: Enqueue,
) where
    Enqueue: FnOnce(u64) -> EnqFut,
    EnqFut: std::future::Future<Output = Result<()>>,
{
    // 1. Allocate the session region, mirroring `rpc::connect`.
    let (request_channel, reply_channel, session_id, _region) =
        match byte_channel::create(SESSION_RING_CAPACITY, SESSION_RING_CAPACITY) {
            Ok(created) => created,
            Err(_) => {
                terminate(&mut stream_write, TERMINATE_ATTACH_FAILED).await;
                return;
            }
        };

    // 3. Rendezvous: enqueue the session id into the served queue. The
    //    serving guest dequeues the handoff and `rpc::accept`s the region.
    if enqueue(session_id).await.is_err() {
        drop(free_region(session_id));
        terminate(&mut stream_write, TERMINATE_ATTACH_FAILED).await;
        return;
    }
    selium_guest::info!("bridge-channel: session {session_id} enqueued");

    // Session adapters: the pipe's client-role membership — one counting
    // writer on the request ring, one blocking reader on the reply ring.
    let session_write = match request_channel.blocking_writer() {
        Ok(writer) => FramedWrite::new(RingWriteTransport { writer }),
        Err(_) => {
            drop(free_region(session_id));
            terminate(&mut stream_write, TERMINATE_ATTACH_FAILED).await;
            return;
        }
    };
    let session_read = match reply_channel.blocking_reader() {
        Ok(reader) => FramedRead::new(RingReadTransport { reader }),
        Err(_) => {
            drop(free_region(session_id));
            terminate(&mut stream_write, TERMINATE_ATTACH_FAILED).await;
            return;
        }
    };

    // 3. Deterministic success reply: the session is enqueued, so the pipe
    //    is established. The serving guest accepts at its own pace; the
    //    accept gates in the pumps order the first relayed frame after
    //    the server's readers (same contract as `rpc::connect`'s
    //    accept wait). Best-effort (a client that vanished needs no reply).
    let accepted = FlatMsg::encode(&PipeControl::Accepted);
    drop(stream_write.write_frame(&accepted, 0));

    // 4. Relay until either half closes; `select!` cancels the loser.
    let to_ring = pump_stream_to_session(stream_read, session_write, request_channel);
    let to_stream = pump_session_to_stream(session_read, stream_write, reply_channel);
    tokio::select! {
        _ = to_ring => {}
        _ = to_stream => {}
    }

    // 5. Teardown: free the session region so the serving guest observes
    //    session end (its ring mappings disappear), mirroring
    //    `OwnedRpcClient`'s drop semantics. Best-effort: a peer that freed
    //    first is fine.
    drop(free_region(session_id));
}

/// Transparent splice into a fabric channel ring (the original pipe):
/// attach, accepted reply, then relay until either half closes.
async fn splice_pipe(
    stream_read: FramedRead<StreamReadTransport>,
    mut stream_write: FramedWrite<StreamWriteTransport>,
    region_id: u64,
) {
    let channel = match Channel::attach(region_id) {
        Ok(channel) => channel,
        Err(_) => {
            terminate(&mut stream_write, TERMINATE_ATTACH_FAILED).await;
            return;
        }
    };

    // Fabric ring adapters, split read/write so the pipe contributes exactly
    // ONE counting writer (the write transport's): inner guests see the
    // pipe's membership via writer_count (its death is visible as a count
    // drop), and the pipe can observe "all inner writers gone" as
    // writer_count == OWN_RING_WRITERS (only itself remains).
    let ring_read = match channel.blocking_reader() {
        Ok(reader) => FramedRead::new(RingReadTransport { reader }),
        Err(_) => {
            terminate(&mut stream_write, TERMINATE_ATTACH_FAILED).await;
            return;
        }
    };
    let ring_write = match channel.blocking_writer() {
        Ok(writer) => FramedWrite::new(RingWriteTransport { writer }),
        Err(_) => {
            terminate(&mut stream_write, TERMINATE_ATTACH_FAILED).await;
            return;
        }
    };

    // Deterministic success reply: the channel is resolved and attached,
    // so the client may treat silence-after-handshake as a protocol
    // violation. Best-effort (a client that vanished needs no reply).
    let accepted = FlatMsg::encode(&PipeControl::Accepted);
    drop(stream_write.write_frame(&accepted, 0));

    // Splice until either half closes; `select!` cancels the loser, whose
    // dropped halves release the fabric membership / finish the stream.
    let to_ring = pump_stream_to_ring(stream_read, ring_write);
    let to_stream = pump_ring_to_stream(ring_read, stream_write, channel);
    tokio::select! {
        _ = to_ring => {}
        _ = to_stream => {}
    }
}

/// Sends a termination frame, best-effort (the writer is dropped after, closing
/// the stream and surfacing EOF to the connector).
async fn terminate(stream_write: &mut FramedWrite<StreamWriteTransport>, code: u32) {
    let payload = FlatMsg::encode(&PipeControl::Terminate { code });
    drop(stream_write.write_frame(&payload, 0));
}

/// Parks until the fabric ring's generation advances, so an inner peer's
/// write or close re-polls the pump.
///
/// Registers the task's waker through the generation-wait mechanism (the
/// same park `BlockingReader` uses); a close bumps the generation too, so
/// the quiesce check in the caller re-runs on every wake. Falls back to a
/// self-wake when no generation callbacks are installed (native tests
/// without the guest runtime), preserving cooperative-yield behaviour.
async fn wait_for_ring_advance(region: &selium_shm::ChannelRegion) {
    let region_id = region.region_id();
    let mut observed = region.load_generation().unwrap_or(0);
    std::future::poll_fn(move |cx| {
        let current = region.load_generation().unwrap_or(0);
        if current != observed {
            observed = current;
            return Poll::Ready(());
        }
        if !selium_memory::register_generation_wait(region_id, observed, cx.waker()) {
            cx.waker().wake_by_ref();
        }
        Poll::Pending
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use selium_abi::RegionProt;
    use selium_memory::FrameHeader;
    use selium_shm::{byte_channel, transport::ShmTransport};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn setup() {
        drop(selium_memory::set_region_provider(Box::new(
            selium_memory::HeapRegionProvider::new(),
        )));
    }

    /// A resolved channel target (the splice path).
    fn channel_target(resource_id: u64) -> ResourceTarget {
        ResourceTarget {
            uri: "sel://acme/lobby".to_string(),
            host_id: String::new(),
            resource_id,
            interface: None,
            tenant: Some("acme".to_string()),
            class: ResourceClass::SharedRegion,
            labels: Vec::new(),
        }
    }

    /// A resolved host-queue target (the rendezvous path).
    fn queue_target(resource_id: u64) -> ResourceTarget {
        ResourceTarget {
            uri: "sel://acme/control".to_string(),
            host_id: String::new(),
            resource_id,
            interface: None,
            tenant: Some("acme".to_string()),
            class: ResourceClass::HostQueue,
            labels: Vec::new(),
        }
    }

    /// A resolver that resolves any URI to the given channel target.
    fn channel_resolver(
        resource_id: u64,
    ) -> impl FnOnce(String) -> std::future::Ready<Result<ResourceTarget>> {
        move |_uri| std::future::ready(Ok(channel_target(resource_id)))
    }

    /// A resolver that always fails (target not found / denied).
    fn failing_resolver() -> impl FnOnce(String) -> std::future::Ready<Result<ResourceTarget>> {
        move |_uri| std::future::ready(Err(GuestError::Host("denied".to_string())))
    }

    /// An enqueue seam for splice-path tests (never invoked).
    fn unused_enqueue() -> impl FnOnce(u64, u64) -> std::future::Ready<Result<()>> {
        |_queue_id, _session_id| std::future::ready(Ok(()))
    }

    fn connector_peer(
        shared_id: u64,
        ring_from_guest: &Channel,
        ring_to_guest: &Channel,
    ) -> ByteStream {
        let region = selium_memory::region_provider()
            .expect("provider")
            .attach(shared_id, None, RegionProt::ReadWrite)
            .expect("attach");
        ByteStream::from_ring_channels(ring_from_guest, ring_to_guest, region, true)
            .expect("connector peer")
    }

    /// 5.2: the delivered region attaches as a `ByteStream`; bytes round-trip
    /// against a connector-style peer half (mirrors the connector `stream.rs`).
    #[tokio::test]
    async fn delivered_region_round_trips_bytes() {
        setup();
        let (ring_to_guest, ring_from_guest, shared_id, _region) =
            byte_channel::create(4096, 4096).expect("create");
        let mut peer = connector_peer(shared_id, &ring_from_guest, &ring_to_guest);
        let mut guest = ByteStream::attach_blocking(shared_id).expect("guest attach");

        peer.write_all(b"request").await.expect("peer write");
        let mut buf = [0u8; 7];
        guest.read_exact(&mut buf).await.expect("guest read");
        assert_eq!(&buf, b"request");

        guest.write_all(b"response").await.expect("guest write");
        let mut buf = [0u8; 8];
        peer.read_exact(&mut buf).await.expect("peer read");
        assert_eq!(&buf, b"response");
    }

    /// 5.5: transport-agnostic framing + channel creation work through the
    /// heap region provider.
    #[test]
    fn channel_and_transport_attach_through_heap_provider() {
        setup();
        let fabric = Channel::create_with_backpressure(
            4096,
            selium_shm::ChannelBackpressure::Park,
            selium_abi::ResourceKind::SharedMemory,
        )
        .expect("create fabric channel");
        let read = ShmTransport::new(&fabric, &fabric).expect("read transport");
        let write = ShmTransport::new(&fabric, &fabric).expect("write transport");

        let mut reader = FramedRead::new(read);
        let mut writer = FramedWrite::new(write);

        writer.write_frame(b"ping", 42).expect("write frame");
        let (payload, tag, flags) = reader.read_frame().expect("read frame");
        assert_eq!(payload, b"ping");
        assert_eq!(tag, 42);
        assert_ne!(flags & FrameHeader::FLAG_READY, 0);
    }

    /// 5.1/5.3 + 5.5: full handshake, resolve-and-attach, tagged frame
    /// round-trip, and client-FIN teardown of the whole pipe.
    #[tokio::test]
    async fn splice_preserves_tags_and_client_fin_tears_down() {
        setup();

        // Fabric channel bridged by the guest, and its inner reader.
        let fabric = Channel::create_with_backpressure(
            4096,
            selium_shm::ChannelBackpressure::Park,
            selium_abi::ResourceKind::SharedMemory,
        )
        .expect("fabric channel");
        let fabric_region_id = fabric.region_id();
        let inner_transport = ShmTransport::new(&fabric, &fabric).expect("inner transport");
        let mut inner_read = FramedRead::new(inner_transport);

        // Relay byte channel between the "connector" (this test) and the guest.
        let (ring_to_guest, ring_from_guest, shared_id, _region) =
            byte_channel::create(65_536, 65_536).expect("create");
        let mut peer = connector_peer(shared_id, &ring_from_guest, &ring_to_guest);
        let guest_stream = ByteStream::attach_blocking(shared_id).expect("guest attach");

        // Drive the guest pipe against the fixed fabric channel.
        let guest_task = tokio::spawn(bridge_pipe(
            guest_stream,
            channel_resolver(fabric_region_id),
            unused_enqueue(),
        ));

        // Client sends the typed handshake frame, then a data frame (tag 7).
        let handshake = FlatMsg::encode(&PipeControl::Handshake {
            uri: "sel://acme/lobby".to_string(),
        });
        write_raw_frame(&mut peer, &handshake, 0, FrameHeader::FLAG_READY)
            .await
            .expect("write handshake");

        let payload = vec![0x42u8; 32];
        write_raw_frame(&mut peer, &payload, 7, FrameHeader::FLAG_READY)
            .await
            .expect("write data");

        // The inner fabric peer observes the frame with tag/flags/payload intact.
        let (relayed, tag, flags) = loop {
            match inner_read.read_frame() {
                Ok(frame) => break frame,
                Err(WireError::BufferEmpty) => tokio::task::yield_now().await,
                Err(e) => panic!("inner read: {e}"),
            }
        };
        assert_eq!(relayed, payload, "payload preserved end-to-end");
        assert_eq!(tag, 7, "correlation tag preserved");
        assert_ne!(flags & FrameHeader::FLAG_READY, 0, "flags preserved");

        // Client FIN: the guest pump exits and the whole pipe tears down.
        drop(peer);
        tokio::time::timeout(std::time::Duration::from_secs(5), guest_task)
            .await
            .expect("guest task completes within timeout")
            .expect("guest task succeeds");
    }

    /// 5.4: a denied/broken resolve yields a typed termination frame.
    #[tokio::test]
    async fn denied_attach_sends_termination_frame() {
        setup();

        let (ring_to_guest, ring_from_guest, shared_id, _region) =
            byte_channel::create(65_536, 65_536).expect("create");
        let mut peer = connector_peer(shared_id, &ring_from_guest, &ring_to_guest);
        let guest_stream = ByteStream::attach_blocking(shared_id).expect("guest attach");

        // Resolver always fails (channel not found / denied).
        let guest_task = tokio::spawn(bridge_pipe(
            guest_stream,
            failing_resolver(),
            unused_enqueue(),
        ));

        // Send a valid handshake; the guest replies with a termination frame
        // then closes the stream.
        let handshake = FlatMsg::encode(&PipeControl::Handshake {
            uri: "sel://acme/forbidden".to_string(),
        });
        write_raw_frame(&mut peer, &handshake, 0, FrameHeader::FLAG_READY)
            .await
            .expect("write handshake");

        tokio::time::timeout(std::time::Duration::from_secs(5), guest_task)
            .await
            .expect("guest task completes")
            .expect("guest task ok");

        // Read the termination frame back out of the stream bytes.
        let mut buf = Vec::new();
        peer.read_to_end(&mut buf).await.expect("read stream");

        // Buffer holds the wire bytes: [byte-channel header was already stripped
        // by the peer's ByteStream reader], so `buf` contains the raw
        // frame the guest sent: [FrameHeader][PipeControl::Terminate payload].
        assert!(
            buf.len() >= FrameHeader::ENCODED_SIZE,
            "termination frame present"
        );
        let header = FrameHeader::decode(&buf[..FrameHeader::ENCODED_SIZE]).expect("frame header");
        assert_eq!(header.tag, 0);
        let payload = &buf[FrameHeader::ENCODED_SIZE..];
        let control: PipeControl = FlatMsg::decode(payload).expect("control frame");
        assert_eq!(
            control,
            PipeControl::Terminate {
                code: TERMINATE_ATTACH_FAILED
            }
        );
    }

    /// 6.2: killing a bridge-channel (dropping its fabric writer) surfaces
    /// `writer_count == 0` to inner guests, without affecting sibling pipes.
    #[test]
    fn dropped_bridge_writer_surfaces_zero_writer_count_without_sibling_effect() {
        setup();
        let fabric = Channel::create_with_backpressure(
            4096,
            selium_shm::ChannelBackpressure::Park,
            selium_abi::ResourceKind::SharedMemory,
        )
        .expect("fabric channel");
        let sibling = Channel::create_with_backpressure(
            4096,
            selium_shm::ChannelBackpressure::Park,
            selium_abi::ResourceKind::SharedMemory,
        )
        .expect("sibling channel");

        // A bridge-channel registers its fabric writer (writer_count = 1).
        let writer = fabric.blocking_writer().expect("fabric writer");
        assert_eq!(
            fabric.ring().region().load_writer_count().expect("count"),
            1
        );
        assert_eq!(
            sibling.ring().region().load_writer_count().expect("count"),
            0
        );

        // Killing the bridge-channel releases the writer.
        drop(writer);
        assert_eq!(
            fabric.ring().region().load_writer_count().expect("count"),
            0,
            "inner guests observe writer_count == 0 after a bridge-channel dies"
        );
        assert_eq!(
            sibling.ring().region().load_writer_count().expect("count"),
            0,
            "other pipes are unaffected"
        );
    }

    /// 5.6 (teardown, fabric-close direction): when the fabric channel
    /// closes (all inner writers gone), the bridge-channel finishes the
    /// client's stream and the whole pipe terminates — the client peer
    /// observes the final frame followed by EOF, not a hang.
    #[tokio::test]
    async fn fabric_close_finishes_client_stream_and_tears_down() {
        setup();

        // Fabric channel bridged by the guest, with an inner writer peer.
        let fabric = Channel::create_with_backpressure(
            4096,
            selium_shm::ChannelBackpressure::Park,
            selium_abi::ResourceKind::SharedMemory,
        )
        .expect("fabric channel");
        let fabric_region_id = fabric.region_id();
        let mut inner_writer = fabric.blocking_writer().expect("inner writer");

        // Relay byte channel between the "connector" (this test) and the guest.
        let (ring_to_guest, ring_from_guest, shared_id, _region) =
            byte_channel::create(65_536, 65_536).expect("create");
        let mut peer = connector_peer(shared_id, &ring_from_guest, &ring_to_guest);
        let guest_stream = ByteStream::attach_blocking(shared_id).expect("guest attach");

        let guest_task = tokio::spawn(bridge_pipe(
            guest_stream,
            channel_resolver(fabric_region_id),
            unused_enqueue(),
        ));

        // Handshake so the pipe attaches the fabric channel.
        let handshake = FlatMsg::encode(&PipeControl::Handshake {
            uri: "sel://acme/lobby".to_string(),
        });
        write_raw_frame(&mut peer, &handshake, 0, FrameHeader::FLAG_READY)
            .await
            .expect("write handshake");

        // Wait until the pipe has attached: its own counting writer brings
        // the fabric writer_count to 2 (inner peer + pipe).
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while fabric.ring().region().load_writer_count().expect("count") < 2 {
            assert!(
                std::time::Instant::now() < deadline,
                "pipe must attach the fabric channel"
            );
            tokio::task::yield_now().await;
        }

        // The inner peer writes one final frame, then leaves (drops its
        // writer — all inner writers gone).
        write_raw_frame(&mut inner_writer, b"bye", 1, FrameHeader::FLAG_READY)
            .await
            .expect("inner write");
        drop(inner_writer);

        // The guest task must terminate on the fabric close (the client peer
        // is still connected, so only the fabric half ended).
        tokio::time::timeout(std::time::Duration::from_secs(5), guest_task)
            .await
            .expect("guest task must tear down on fabric close")
            .expect("guest task succeeds");

        // The client observes the accepted handshake reply, then the relayed
        // frame, followed by stream EOF.
        let mut buf = Vec::new();
        let n = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            use tokio::io::AsyncReadExt;
            peer.read_to_end(&mut buf).await
        })
        .await
        .expect("peer must observe stream finish promptly")
        .expect("peer read must succeed");
        assert!(n > 0, "relayed frame must reach the client before EOF");

        // First frame: the deterministic accepted reply (tag 0).
        assert!(
            buf.len() >= 2 * FrameHeader::ENCODED_SIZE,
            "accepted + fabric frames present"
        );
        let accepted_header =
            FrameHeader::decode(&buf[..FrameHeader::ENCODED_SIZE]).expect("accepted header");
        assert_eq!(
            accepted_header.tag, 0,
            "accepted reply carries control tag 0"
        );
        let accepted_payload = &buf[FrameHeader::ENCODED_SIZE..accepted_frame_end(&buf)];
        assert_eq!(
            <PipeControl as FlatMsg>::decode(accepted_payload).expect("accepted control frame"),
            PipeControl::Accepted,
            "successful handshake replies with an accepted frame"
        );

        // Second frame: the fabric frame relayed before the stream finish.
        let fabric_frame = &buf[accepted_frame_end(&buf)..];
        let header =
            FrameHeader::decode(&fabric_frame[..FrameHeader::ENCODED_SIZE]).expect("header");
        assert_eq!(header.tag, 1, "fabric frame relayed before finish");
    }

    /// The handshake is deterministic: a successful resolve/attach replies
    /// with a typed accepted control frame before the relay begins, so an
    /// external client can surface handshake failures at channel open.
    #[tokio::test]
    async fn successful_handshake_replies_accepted_frame() {
        setup();

        // Fabric channel bridged by the guest (no inner peers needed).
        let fabric = Channel::create_with_backpressure(
            4096,
            selium_shm::ChannelBackpressure::Park,
            selium_abi::ResourceKind::SharedMemory,
        )
        .expect("fabric channel");
        let fabric_region_id = fabric.region_id();

        let (ring_to_guest, ring_from_guest, shared_id, _region) =
            byte_channel::create(65_536, 65_536).expect("create");
        let mut peer = connector_peer(shared_id, &ring_from_guest, &ring_to_guest);
        let guest_stream = ByteStream::attach_blocking(shared_id).expect("guest attach");

        let guest_task = tokio::spawn(bridge_pipe(
            guest_stream,
            channel_resolver(fabric_region_id),
            unused_enqueue(),
        ));

        let handshake = FlatMsg::encode(&PipeControl::Handshake {
            uri: "sel://acme/lobby".to_string(),
        });
        write_raw_frame(&mut peer, &handshake, 0, FrameHeader::FLAG_READY)
            .await
            .expect("write handshake");

        // The client peer receives the accepted reply (the pipe keeps
        // splicing until the client hangs up).
        let mut buf = Vec::new();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            use tokio::io::AsyncReadExt;
            let mut chunk = [0u8; 4096];
            match peer.read(&mut chunk).await {
                Ok(0) => panic!("pipe closed before replying"),
                Ok(n) => {
                    buf.extend_from_slice(&chunk[..n]);
                    if buf.len() >= FrameHeader::ENCODED_SIZE {
                        let header = FrameHeader::decode(&buf[..FrameHeader::ENCODED_SIZE])
                            .expect("reply header");
                        if buf.len() >= FrameHeader::ENCODED_SIZE + header.len as usize {
                            break;
                        }
                    }
                }
                Err(_) => panic!("peer read failed"),
            }
            assert!(
                std::time::Instant::now() < deadline,
                "accepted reply timeout"
            );
        }
        let header = FrameHeader::decode(&buf[..FrameHeader::ENCODED_SIZE]).expect("header");
        assert_eq!(header.tag, 0, "accepted reply carries control tag 0");
        let payload =
            &buf[FrameHeader::ENCODED_SIZE..FrameHeader::ENCODED_SIZE + header.len as usize];
        assert_eq!(
            <PipeControl as FlatMsg>::decode(payload).expect("control frame"),
            PipeControl::Accepted
        );

        drop(peer);
        tokio::time::timeout(std::time::Duration::from_secs(5), guest_task)
            .await
            .expect("guest task completes within timeout")
            .expect("guest task succeeds");
    }

    /// Returns the end offset of the first complete frame in `buf`, or panics
    /// if the frame is truncated.
    fn accepted_frame_end(buf: &[u8]) -> usize {
        assert!(
            buf.len() >= FrameHeader::ENCODED_SIZE,
            "frame header present"
        );
        let header = FrameHeader::decode(&buf[..FrameHeader::ENCODED_SIZE]).expect("header");
        FrameHeader::ENCODED_SIZE + header.len as usize
    }

    /// 6.2 (uplift): killing a bridge-channel mid-pipe (here: aborting the
    /// pipe task, the guest-level equivalent of a supervisor kill) drops its
    /// fabric membership — inner guests observe `writer_count == 0` — while a
    /// sibling pipe on another fabric channel is unaffected and keeps
    /// relaying.
    #[tokio::test]
    async fn killing_bridge_pipe_mid_stream_is_isolated_from_siblings() {
        setup();

        // Fabric channel for the killed pipe, with an inner reader peer
        // (writerless: the bridge-channel is the ring's only writer, so the
        // inner guest observes `writer_count == 0` when the pipe dies).
        let fabric = Channel::create_with_backpressure(
            4096,
            selium_shm::ChannelBackpressure::Park,
            selium_abi::ResourceKind::SharedMemory,
        )
        .expect("fabric channel");
        let fabric_region_id = fabric.region_id();
        let _fabric_inner_reader = fabric.blocking_reader().expect("inner reader");

        // Sibling fabric channel with its own bridge pipe and a full inner
        // peer (reader + writer), so the sibling keeps relaying.
        let sibling = Channel::create_with_backpressure(
            4096,
            selium_shm::ChannelBackpressure::Park,
            selium_abi::ResourceKind::SharedMemory,
        )
        .expect("sibling fabric channel");
        let sibling_region_id = sibling.region_id();
        // Full inner peer on the sibling fabric (blocking reader + counting
        // writer), created before the pipe attaches so the pipe's quiesce
        // check always sees another member.
        let mut sibling_read =
            FramedRead::new(ShmTransport::new(&sibling, &sibling).expect("sibling inner"));

        // Both pipes are bridged from their own relay byte channels.
        let (ring_to_guest, ring_from_guest, shared_id, _region) =
            byte_channel::create(65_536, 65_536).expect("create");
        let mut peer = connector_peer(shared_id, &ring_from_guest, &ring_to_guest);
        let guest_stream = ByteStream::attach_blocking(shared_id).expect("guest attach");
        let killed_task = tokio::spawn(bridge_pipe(
            guest_stream,
            channel_resolver(fabric_region_id),
            unused_enqueue(),
        ));

        let (s_ring_to, s_ring_from, s_shared, _s_region) =
            byte_channel::create(65_536, 65_536).expect("create sibling channel");
        let mut sibling_peer = connector_peer(s_shared, &s_ring_from, &s_ring_to);
        let sibling_stream = ByteStream::attach_blocking(s_shared).expect("sibling attach");
        let sibling_task = tokio::spawn(bridge_pipe(
            sibling_stream,
            channel_resolver(sibling_region_id),
            unused_enqueue(),
        ));

        // Both pipes complete the handshake.
        let handshake = FlatMsg::encode(&PipeControl::Handshake {
            uri: "sel://acme/lobby".to_string(),
        });
        write_raw_frame(&mut peer, &handshake, 0, FrameHeader::FLAG_READY)
            .await
            .expect("killed pipe handshake");
        write_raw_frame(&mut sibling_peer, &handshake, 0, FrameHeader::FLAG_READY)
            .await
            .expect("sibling pipe handshake");

        // Wait until both pipes have attached their fabric channels: the
        // killed pipe registers its writer on the writerless fabric (count
        // 0 → 1); the sibling pipe brings its fabric to 2 (inner peer +
        // pipe).
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while fabric.ring().region().load_writer_count().expect("count") < 1
            || sibling.ring().region().load_writer_count().expect("count") < 2
        {
            assert!(
                std::time::Instant::now() < deadline,
                "both pipes must attach their fabric channels"
            );
            tokio::task::yield_now().await;
        }

        // Kill the first pipe mid-stream (supervisor-kill analogue).
        killed_task.abort();

        // The killed pipe's fabric membership is gone: inner guests observe
        // `writer_count == 0`.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while fabric.ring().region().load_writer_count().expect("count") != 0 {
            assert!(
                std::time::Instant::now() < deadline,
                "killed pipe must release its fabric writer"
            );
            tokio::task::yield_now().await;
        }
        assert_eq!(
            fabric.ring().region().load_writer_count().expect("count"),
            0,
            "inner guests observe writer_count == 0 after the kill"
        );

        // The sibling pipe is unaffected: it still relays frames.
        let payload = b"still-alive".to_vec();
        write_raw_frame(&mut sibling_peer, &payload, 9, FrameHeader::FLAG_READY)
            .await
            .expect("sibling data frame");

        let (relayed, tag, _) = loop {
            match sibling_read.read_frame() {
                Ok(frame) => break frame,
                Err(WireError::BufferEmpty) => tokio::task::yield_now().await,
                Err(e) => panic!("sibling inner read: {e}"),
            }
        };
        assert_eq!(relayed, payload, "sibling pipe still relays after the kill");
        assert_eq!(tag, 9);

        // Clean teardown of the sibling.
        drop(sibling_peer);
        tokio::time::timeout(std::time::Duration::from_secs(5), sibling_task)
            .await
            .expect("sibling pipe tears down on client FIN")
            .expect("sibling pipe succeeds");
    }

    /// Encodes + writes one frame onto a byte channel in its raw frame format.
    async fn write_raw_frame<W: tokio::io::AsyncWrite + Unpin>(
        writer: &mut W,
        payload: &[u8],
        tag: u32,
        flags: u8,
    ) -> std::io::Result<()> {
        let header = FrameHeader {
            len: payload.len() as u32,
            tag,
            flags,
        };
        let mut framed = Vec::with_capacity(FrameHeader::ENCODED_SIZE + payload.len());
        framed.extend_from_slice(&header.encode());
        framed.extend_from_slice(payload);
        writer.write_all(&framed).await
    }

    /// Reads one complete frame from a byte channel, returning its payload,
    /// correlation tag, and flags.
    async fn read_raw_frame<R: tokio::io::AsyncRead + Unpin>(
        reader: &mut R,
    ) -> std::io::Result<(Vec<u8>, u32, u8)> {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            if buf.len() >= FrameHeader::ENCODED_SIZE {
                let header =
                    FrameHeader::decode(&buf[..FrameHeader::ENCODED_SIZE]).expect("frame header");
                let end = FrameHeader::ENCODED_SIZE + header.len as usize;
                if buf.len() >= end {
                    let payload = buf[FrameHeader::ENCODED_SIZE..end].to_vec();
                    return Ok((payload, header.tag, header.flags));
                }
            }
            let n = reader.read(&mut chunk).await?;
            if n == 0 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "frame truncated",
                ));
            }
            buf.extend_from_slice(&chunk[..n]);
        }
    }

    /// 8.2: an external RPC client reaches a served host queue through the
    /// rendezvous — the pipe allocates the session region, enqueues it into
    /// the served queue, and relays a correlated typed request/reply
    /// between the stream and the serving guest's `rpc::accept` session.
    #[tokio::test]
    async fn rendezvous_round_trips_typed_rpc_frames_and_frees_session() {
        setup();

        // Served-queue stand-in: the enqueue seam records the handoff the
        // way a real queue delivers it to the serving guest.
        let (handoff_tx, mut handoff_rx) = tokio::sync::mpsc::unbounded_channel::<(u64, u64)>();
        const QUEUE_ID: u64 = 4242;

        // Relay byte channel between the "connector" (this test) and the guest.
        let (ring_to_guest, ring_from_guest, shared_id, _region) =
            byte_channel::create(65_536, 65_536).expect("create");
        let mut peer = connector_peer(shared_id, &ring_from_guest, &ring_to_guest);
        let guest_stream = ByteStream::attach_blocking(shared_id).expect("guest attach");

        let guest_task = tokio::spawn(bridge_pipe(
            guest_stream,
            // The served target is a host-queue listener (e.g. the control
            // plane's `control.<tenant>` route).
            move |_uri| std::future::ready(Ok(queue_target(QUEUE_ID))),
            move |queue_id, session_id| async move {
                handoff_tx
                    .send((queue_id, session_id))
                    .map_err(|error| GuestError::Host(format!("handoff channel closed: {error}")))
            },
        ));

        // Client handshake naming the control route.
        let handshake = FlatMsg::encode(&PipeControl::Handshake {
            uri: "sel://acme/control".to_string(),
        });
        write_raw_frame(&mut peer, &handshake, 0, FrameHeader::FLAG_READY)
            .await
            .expect("write handshake");

        // The serving guest dequeues the handoff: the session was enqueued
        // into the served queue under the right queue id.
        let (queue_id, session_id) = handoff_rx.recv().await.expect("handoff delivered");
        assert_eq!(queue_id, QUEUE_ID);

        // The serving guest accepts the session (the control plane's
        // `rpc::accept` path).
        let mut server: selium_shm::rpc::RpcConnection<String, String> =
            selium_shm::rpc::accept(selium_wire::rpc::IncomingConnection {
                client_process_id: 7,
                shared_id: session_id,
            })
            .expect("server accept");
        let server_task = tokio::spawn(async move {
            let request = server.recv().await.expect("server recv");
            assert_eq!(request.payload().expect("server decode"), "ping");
            request.reply("pong".to_string()).await.expect("reply");
        });

        // The external client's request frame (correlation tag 7).
        write_raw_frame(&mut peer, b"ping", 7, FrameHeader::FLAG_READY)
            .await
            .expect("write request");
        server_task.await.expect("server task");

        // The client receives the deterministic accepted reply, then the
        // correlated response.
        let (payload, tag, _) = read_raw_frame(&mut peer).await.expect("accepted frame");
        assert_eq!(
            <PipeControl as FlatMsg>::decode(&payload).expect("control frame"),
            PipeControl::Accepted
        );
        assert_eq!(tag, 0);
        let (payload, tag, _) = read_raw_frame(&mut peer).await.expect("reply frame");
        assert_eq!(payload, b"pong".to_vec());
        assert_eq!(tag, 7, "correlation tag preserved through the session");

        // Client FIN: the pipe tears down and frees the session region, so
        // the serving guest observes session end rather than a leak.
        drop(peer);
        tokio::time::timeout(std::time::Duration::from_secs(5), guest_task)
            .await
            .expect("guest task completes within timeout")
            .expect("guest task succeeds");
        assert!(
            byte_channel::attach(session_id).is_err(),
            "session region freed on teardown"
        );
    }

    /// 8.2/8.3: an enqueue failure (queue unavailable / attach denied)
    /// yields the typed termination frame and reclaims the session region.
    #[tokio::test]
    async fn rendezvous_enqueue_failure_terminates_and_frees_session() {
        setup();

        let (session_tx, mut session_rx) = tokio::sync::mpsc::unbounded_channel::<u64>();

        let (ring_to_guest, ring_from_guest, shared_id, _region) =
            byte_channel::create(65_536, 65_536).expect("create");
        let mut peer = connector_peer(shared_id, &ring_from_guest, &ring_to_guest);
        let guest_stream = ByteStream::attach_blocking(shared_id).expect("guest attach");

        let guest_task = tokio::spawn(bridge_pipe(
            guest_stream,
            move |_uri| std::future::ready(Ok(queue_target(77))),
            move |_queue_id, session_id| async move {
                // Record the session so the test can observe the reclaim,
                // then fail the handoff.
                session_tx.send(session_id).expect("record session id");
                Err(GuestError::Host("queue unavailable".to_string()))
            },
        ));

        let handshake = FlatMsg::encode(&PipeControl::Handshake {
            uri: "sel://acme/control".to_string(),
        });
        write_raw_frame(&mut peer, &handshake, 0, FrameHeader::FLAG_READY)
            .await
            .expect("write handshake");

        tokio::time::timeout(std::time::Duration::from_secs(5), guest_task)
            .await
            .expect("guest task completes within timeout")
            .expect("guest task succeeds");

        // The session region is reclaimed after the failed handoff.
        let session_id = session_rx.recv().await.expect("session id recorded");
        assert!(
            byte_channel::attach(session_id).is_err(),
            "session region reclaimed on enqueue failure"
        );

        // The client observes the typed termination frame then EOF.
        let mut buf = Vec::new();
        peer.read_to_end(&mut buf).await.expect("read stream");
        assert!(
            buf.len() >= FrameHeader::ENCODED_SIZE,
            "termination frame present"
        );
        let header = FrameHeader::decode(&buf[..FrameHeader::ENCODED_SIZE]).expect("frame header");
        assert_eq!(header.tag, 0);
        let payload = &buf[FrameHeader::ENCODED_SIZE..accepted_frame_end(&buf)];
        assert_eq!(
            <PipeControl as FlatMsg>::decode(payload).expect("control frame"),
            PipeControl::Terminate {
                code: TERMINATE_ATTACH_FAILED
            }
        );
    }

    /// 8.3 (server-side session end): when the serving guest ends the
    /// session after serving a request, the client's stream finishes
    /// without a client FIN — the pipe observes the reply ring quiesce,
    /// tears down, and frees the session region.
    #[tokio::test]
    async fn server_session_end_finishes_client_stream() {
        setup();

        let (handoff_tx, mut handoff_rx) = tokio::sync::mpsc::unbounded_channel::<(u64, u64)>();

        let (ring_to_guest, ring_from_guest, shared_id, _region) =
            byte_channel::create(65_536, 65_536).expect("create");
        let mut peer = connector_peer(shared_id, &ring_from_guest, &ring_to_guest);
        let guest_stream = ByteStream::attach_blocking(shared_id).expect("guest attach");

        let guest_task = tokio::spawn(bridge_pipe(
            guest_stream,
            move |_uri| std::future::ready(Ok(queue_target(4242))),
            move |queue_id, session_id| async move {
                handoff_tx
                    .send((queue_id, session_id))
                    .map_err(|error| GuestError::Host(format!("handoff channel closed: {error}")))
            },
        ));

        let handshake = FlatMsg::encode(&PipeControl::Handshake {
            uri: "sel://acme/control".to_string(),
        });
        write_raw_frame(&mut peer, &handshake, 0, FrameHeader::FLAG_READY)
            .await
            .expect("write handshake");

        let (_, session_id) = handoff_rx.recv().await.expect("handoff delivered");

        let mut server: selium_shm::rpc::RpcConnection<String, String> =
            selium_shm::rpc::accept(selium_wire::rpc::IncomingConnection {
                client_process_id: 7,
                shared_id: session_id,
            })
            .expect("server accept");
        // Serve one request, then end the session by dropping the
        // connection (the serving guest's loop exit path).
        let server_task = tokio::spawn(async move {
            let request = server.recv().await.expect("server recv");
            request.reply("done".to_string()).await.expect("reply");
        });

        write_raw_frame(&mut peer, b"ping", 3, FrameHeader::FLAG_READY)
            .await
            .expect("write request");
        server_task.await.expect("server task");

        // No client FIN: the pipe must tear down on the session end alone.
        tokio::time::timeout(std::time::Duration::from_secs(5), guest_task)
            .await
            .expect("pipe tears down on session end")
            .expect("guest task succeeds");
        assert!(
            byte_channel::attach(session_id).is_err(),
            "session region freed on session end"
        );

        // The client observed the accepted reply, the response frame, and
        // then stream EOF (finished by the pipe).
        let mut buf = Vec::new();
        peer.read_to_end(&mut buf).await.expect("read stream");
        assert!(
            buf.len() >= 2 * FrameHeader::ENCODED_SIZE,
            "accepted + response frames present before EOF"
        );
    }
}
