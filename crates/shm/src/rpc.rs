//! Shared-memory RPC session helpers.
//!
//! This module builds on [`selium_wire::rpc`] to provide a concrete
//! shared-memory transport implementation. It preserves the existing
//! multi-memory region layout: one parent shared region contains a small
//! header describing two sub-memories (request ring and reply ring). The
//! client allocates the region and sends the parent `shared_id` to the
//! server via a [`Rendezvous`]; the server attaches to the same region and
//! parses the header to discover the rings.

use std::{
    mem::ManuallyDrop,
    ops::{Deref, DerefMut},
};

use selium_service::FlatMsg;
use selium_wire::{
    error::{Error, Result},
    framed::{FramedRead, FramedWrite},
    rpc::{
        IncomingConnection, Rendezvous, RpcClient as WireRpcClient,
        RpcConnection as WireRpcConnection, RpcRequest as WireRpcRequest,
    },
    stream::{
        RpcServerStreamClient as WireServerStreamClient,
        RpcServerStreamConnection as WireServerStreamConnection,
        RpcServerStreamRequest as WireServerStreamRequest,
    },
};

use crate::{Channel, transport::ShmTransport};

pub use selium_wire::rpc::RpcError;

/// Client-side handle for typed RPC requests over shared memory.
pub type RpcClient<Req, Rep> = WireRpcClient<Req, Rep, ShmTransport>;
/// Server-side handle for an established shared-memory RPC session.
pub type RpcConnection<Req, Rep> = WireRpcConnection<Req, Rep, ShmTransport>;
/// A single request received by the server, with the ability to reply.
pub type RpcRequest<'a, Req, Rep> = WireRpcRequest<'a, Req, Rep, ShmTransport>;
/// Client-side handle for a server-streaming RPC session over shared memory.
pub type ServerStreamClient<Req, Item> = WireServerStreamClient<Req, Item, ShmTransport>;
/// Server-side handle for an established server-streaming RPC session.
pub type ServerStreamConnection<Req, Item> = WireServerStreamConnection<Req, Item, ShmTransport>;
/// A server-streaming request received from a client, with the ability to
/// send stream items.
pub type ServerStreamRequest<'a, Req, Item> = WireServerStreamRequest<'a, Req, Item, ShmTransport>;

/// Default reply ring capacity in bytes.
const DEFAULT_REP_CAPACITY: u64 = 4096;
/// Default request ring capacity in bytes.
const DEFAULT_REQ_CAPACITY: u64 = 4096;

/// An [`RpcClient`] that owns its session region.
///
/// [`connect`] allocates the request/reply region pair client-side; this
/// wrapper frees the parent region via the global region provider when
/// dropped, reclaiming the session's shared memory. The free is
/// ownership-checked by the runtime in guest mode (the allocator is the
/// owner), so dropping a non-owning handle cannot destroy a foreign
/// region. Dropping the inner client before freeing guarantees ring
/// teardown never touches unmapped memory.
pub struct OwnedRpcClient<Req, Rep> {
    inner: ManuallyDrop<RpcClient<Req, Rep>>,
    shared_id: u64,
}

/// Yields once and re-checks, used by [`wait_for_server_accept`].
struct YieldOnce(bool);

/// A [`ServerStreamClient`] that owns its session region.
///
/// Mirrors [`OwnedRpcClient`] for server-streaming sessions: the session
/// region pair is freed via the global region provider when dropped.
pub struct OwnedServerStreamClient<Req, Item> {
    inner: ManuallyDrop<ServerStreamClient<Req, Item>>,
    shared_id: u64,
}

impl<Req, Rep> OwnedRpcClient<Req, Rep> {
    /// Returns the parent shared id of the owned session region.
    pub fn shared_id(&self) -> u64 {
        self.shared_id
    }
}

impl<Req, Rep> Deref for OwnedRpcClient<Req, Rep> {
    type Target = RpcClient<Req, Rep>;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl<Req, Rep> DerefMut for OwnedRpcClient<Req, Rep> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}

impl<Req, Rep> Drop for OwnedRpcClient<Req, Rep> {
    fn drop(&mut self) {
        // SAFETY: `inner` is not used again after this point; dropping it
        // first ensures all channel state is torn down while the mapping
        // is still valid.
        unsafe { ManuallyDrop::drop(&mut self.inner) };
        if crate::free_region(self.shared_id).is_err() {
            // Already reclaimed (e.g. peer freed it first, or process
            // teardown raced); nothing further to do during drop.
        }
    }
}

impl std::future::Future for YieldOnce {
    type Output = ();

    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<()> {
        if self.0 {
            std::task::Poll::Ready(())
        } else {
            self.0 = true;
            cx.waker().wake_by_ref();
            std::task::Poll::Pending
        }
    }
}

impl<Req, Item> OwnedServerStreamClient<Req, Item> {
    /// Returns the parent shared id of the owned session region.
    pub fn shared_id(&self) -> u64 {
        self.shared_id
    }
}

impl<Req, Item> Deref for OwnedServerStreamClient<Req, Item> {
    type Target = ServerStreamClient<Req, Item>;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl<Req, Item> DerefMut for OwnedServerStreamClient<Req, Item> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}

impl<Req, Item> Drop for OwnedServerStreamClient<Req, Item> {
    fn drop(&mut self) {
        // SAFETY: `inner` is not used again after this point; dropping it
        // first ensures all channel state is torn down while the mapping
        // is still valid.
        unsafe { ManuallyDrop::drop(&mut self.inner) };
        if crate::free_region(self.shared_id).is_err() {
            // Already reclaimed (e.g. peer freed it first, or process
            // teardown raced); nothing further to do during drop.
        }
    }
}

/// Accepts an incoming shared-memory RPC session.
///
/// Attaches to the multi-memory region identified by `connection.shared_id`,
/// parses the request/reply ring layout, and returns a typed
/// [`RpcConnection`] for the server side.
pub fn accept<Req, Rep>(
    connection: IncomingConnection,
) -> std::result::Result<RpcConnection<Req, Rep>, RpcError>
where
    Req: FlatMsg,
    Rep: FlatMsg,
{
    let (request_channel, reply_channel) = attach_rpc_region(connection.shared_id)?;

    let request_transport =
        ShmTransport::new(&request_channel, &request_channel).map_err(map_transport_error)?;
    let reply_transport =
        ShmTransport::new(&reply_channel, &reply_channel).map_err(map_transport_error)?;

    Ok(RpcConnection::new(
        FramedRead::new(request_transport),
        FramedWrite::new(reply_transport),
        connection.client_process_id,
    ))
}

/// Accepts an incoming shared-memory server-streaming RPC session.
///
/// Attaches to the multi-memory region identified by `connection.shared_id`,
/// parses the request/reply ring layout, and returns a typed
/// [`ServerStreamConnection`] for the server side.
pub fn accept_server_stream<Req, Item>(
    connection: IncomingConnection,
) -> std::result::Result<ServerStreamConnection<Req, Item>, RpcError>
where
    Req: FlatMsg,
    Item: FlatMsg,
{
    let (request_channel, reply_channel) = attach_rpc_region(connection.shared_id)?;

    let request_transport =
        ShmTransport::new(&request_channel, &request_channel).map_err(map_transport_error)?;
    let reply_transport =
        ShmTransport::new(&reply_channel, &reply_channel).map_err(map_transport_error)?;

    Ok(ServerStreamConnection::new(
        FramedRead::new(request_transport),
        FramedWrite::new(reply_transport),
        connection.client_process_id,
    ))
}

/// Creates a new shared-memory RPC client.
///
/// Allocates a multi-memory region with a request ring and a reply ring,
/// sends the parent `shared_id` to the server through `rendezvous`, and
/// returns an [`OwnedRpcClient`] ready for requests. Dropping the client
/// frees the session region pair.
pub async fn connect<Req, Rep, R>(
    rendezvous: R,
    request_capacity: u64,
    reply_capacity: u64,
) -> std::result::Result<OwnedRpcClient<Req, Rep>, RpcError>
where
    Req: FlatMsg,
    Rep: FlatMsg,
    R: Rendezvous,
{
    let req_cap = if request_capacity == 0 {
        DEFAULT_REQ_CAPACITY
    } else {
        request_capacity
    };
    let rep_cap = if reply_capacity == 0 {
        DEFAULT_REP_CAPACITY
    } else {
        reply_capacity
    };

    let (request_channel, reply_channel, shared_id) = create_rpc_region(req_cap, rep_cap)?;

    let build_client = || -> std::result::Result<RpcClient<Req, Rep>, RpcError> {
        let request_transport =
            ShmTransport::new(&request_channel, &request_channel).map_err(map_transport_error)?;
        let reply_transport =
            ShmTransport::new(&reply_channel, &reply_channel).map_err(map_transport_error)?;
        Ok(WireRpcClient::new(
            FramedWrite::new(request_transport),
            FramedRead::new(reply_transport),
        ))
    };

    match rendezvous.send(shared_id).await {
        Ok(()) => {}
        Err(error) => {
            // Rendezvous failed: reclaim the region pair before returning.
            if crate::free_region(shared_id).is_err() {
                // Best-effort reclaim; surface the rendezvous error instead.
            }
            return Err(RpcError::Serialization(format!(
                "rendezvous send failed: {error}"
            )));
        }
    }

    let client = match build_client() {
        Ok(client) => client,
        Err(error) => {
            if crate::free_region(shared_id).is_err() {
                // Best-effort reclaim; surface the transport error instead.
            }
            return Err(error);
        }
    };

    wait_for_server_accept(&request_channel).await?;

    Ok(OwnedRpcClient {
        inner: ManuallyDrop::new(client),
        shared_id,
    })
}

/// Creates a new shared-memory server-streaming RPC client.
///
/// Allocates a multi-memory region with a request ring and a reply ring,
/// sends the parent `shared_id` to the server through `rendezvous`, and
/// returns an [`OwnedServerStreamClient`] ready for streaming calls.
/// Dropping the client frees the session region pair.
pub async fn connect_server_stream<Req, Item, R>(
    rendezvous: R,
    request_capacity: u64,
    reply_capacity: u64,
) -> std::result::Result<OwnedServerStreamClient<Req, Item>, RpcError>
where
    Req: FlatMsg,
    Item: FlatMsg,
    R: Rendezvous,
{
    let req_cap = if request_capacity == 0 {
        DEFAULT_REQ_CAPACITY
    } else {
        request_capacity
    };
    let rep_cap = if reply_capacity == 0 {
        DEFAULT_REP_CAPACITY
    } else {
        reply_capacity
    };

    let (request_channel, reply_channel, shared_id) = create_rpc_region(req_cap, rep_cap)?;

    let build_client = || -> std::result::Result<ServerStreamClient<Req, Item>, RpcError> {
        let request_transport =
            ShmTransport::new(&request_channel, &request_channel).map_err(map_transport_error)?;
        let reply_transport =
            ShmTransport::new(&reply_channel, &reply_channel).map_err(map_transport_error)?;
        Ok(WireServerStreamClient::new(
            FramedWrite::new(request_transport),
            FramedRead::new(reply_transport),
        ))
    };

    match rendezvous.send(shared_id).await {
        Ok(()) => {}
        Err(error) => {
            // Rendezvous failed: reclaim the region pair before returning.
            if crate::free_region(shared_id).is_err() {
                // Best-effort reclaim; surface the rendezvous error instead.
            }
            return Err(RpcError::Serialization(format!(
                "rendezvous send failed: {error}"
            )));
        }
    }

    let client = match build_client() {
        Ok(client) => client,
        Err(error) => {
            if crate::free_region(shared_id).is_err() {
                // Best-effort reclaim; surface the transport error instead.
            }
            return Err(error);
        }
    };

    wait_for_server_accept(&request_channel).await?;

    Ok(OwnedServerStreamClient {
        inner: ManuallyDrop::new(client),
        shared_id,
    })
}

/// Attaches to an RPC region by `shared_id` and extracts the two ring channels.
fn attach_rpc_region(shared_id: u64) -> Result<(Channel, Channel)> {
    crate::byte_channel::attach(shared_id)
}

/// Creates a multi-memory region with two ring buffers for RPC.
///
/// Returns the request channel, reply channel, and parent `shared_id`. The
/// parent region handle is dropped: the channels hold their own mappings,
/// so the allocation's attachment outlives it (see
/// [`byte_channel::create`], which documents the no-second-attach rule).
fn create_rpc_region(req_capacity: u64, rep_capacity: u64) -> Result<(Channel, Channel, u64)> {
    let (request_channel, reply_channel, shared_id, _region) =
        crate::byte_channel::create(req_capacity, rep_capacity)?;
    Ok((request_channel, reply_channel, shared_id))
}

/// Maps a transport error to an [`RpcError`].
fn map_transport_error(error: selium_wire::error::Error) -> RpcError {
    match error {
        Error::BufferFull => RpcError::BufferFull,
        Error::BufferEmpty => RpcError::BufferEmpty,
        Error::InvalidLayout | Error::InvalidRegion => RpcError::InvalidRegion,
        Error::ConnectionLost | Error::ChannelClosed | Error::Terminated => {
            RpcError::ConnectionClosed
        }
        Error::SerializationFailed(message) | Error::Guest(message) => {
            RpcError::Serialization(message)
        }
        other => RpcError::Serialization(other.to_string()),
    }
}

/// Waits until the serving side has accepted the session.
///
/// A blocking reader starts at the ring tail *at registration time*, so any
/// frame written before the server registers its reader is invisible to the
/// server forever. Waiting here makes first-frame ordering deterministic:
/// the client cannot write its first request before the serving side has a
/// reader positioned at the (empty) ring start.
///
/// The accept signal is the writer count on the request channel. The client
/// contributes exactly one writer (its request transport's write half); the
/// server contributes a second when it builds its request transport during
/// accept. Because a transport registers its eager reader before its writer,
/// observing the second writer guarantees the server's reader is already in
/// place. If the server never accepts, the caller parks here — the honest
/// backpressure outcome for a route nobody serves.
async fn wait_for_server_accept(channel: &Channel) -> Result<()> {
    while channel.ring().region().load_writer_count()? < 2 {
        YieldOnce(false).await;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() {
        crate::install_heap_provider();
    }

    #[test]
    fn create_and_attach_rpc_region_round_trip() {
        // Task 2.1: Verify that create_rpc_region produces a multi-memory
        // region that attach_rpc_region can attach to. The connector calls
        // rpc::connect (create_rpc_region + rendezvous) and the app guest
        // calls rpc::accept (attach_rpc_region).
        setup();

        let (req_ch, rep_ch, shared_id) = create_rpc_region(4096, 4096).expect("create_rpc_region");

        // Parent shared_id must be valid (non-zero).
        assert!(shared_id > 0, "parent shared_id must be non-zero");

        // Attach to the same region (simulates rpc::accept server side).
        let (attached_req, attached_rep) = attach_rpc_region(shared_id).expect("attach_rpc_region");

        // Both created and attached channels should use the same capacity.
        assert_eq!(attached_req.ring().capacity(), req_ch.ring().capacity());
        assert_eq!(attached_rep.ring().capacity(), rep_ch.ring().capacity());
    }

    #[derive(Debug, Clone, PartialEq)]
    struct PingMsg(String);

    #[derive(Debug, Clone, PartialEq)]
    struct PongMsg(String);

    impl selium_service::FlatMsg for PingMsg {
        fn encode(value: &Self) -> Vec<u8> {
            value.0.clone().into_bytes()
        }

        fn decode(bytes: &[u8]) -> std::result::Result<Self, flatbuffers::InvalidFlatbuffer> {
            Ok(Self(String::from_utf8(bytes.to_vec()).map_err(
                |_error| flatbuffers::InvalidFlatbuffer::ApparentSizeTooLarge,
            )?))
        }
    }

    impl selium_service::FlatMsg for PongMsg {
        fn encode(value: &Self) -> Vec<u8> {
            value.0.clone().into_bytes()
        }

        fn decode(bytes: &[u8]) -> std::result::Result<Self, flatbuffers::InvalidFlatbuffer> {
            Ok(Self(String::from_utf8(bytes.to_vec()).map_err(
                |_error| flatbuffers::InvalidFlatbuffer::ApparentSizeTooLarge,
            )?))
        }
    }

    #[tokio::test]
    async fn unary_connect_accept_round_trip() {
        // Unary mirror of the streaming round-trip: connect allocates the
        // region pair, accept attaches it, one request -> one reply.
        setup();

        let rendezvous = crate::ShmRendezvous::new();

        let server = {
            let rendezvous = rendezvous.clone();
            tokio::spawn(async move {
                let incoming = loop {
                    match rendezvous.recv().await {
                        Ok(connection) => break connection,
                        Err(_) => tokio::task::yield_now().await,
                    }
                };
                let mut conn: RpcConnection<PingMsg, PongMsg> = accept(incoming).expect("accept");
                let req = conn.recv().await.expect("recv request");
                assert_eq!(req.payload().expect("decode request").0, "ping");
                req.reply(PongMsg("pong".to_string())).await.expect("reply");
            })
        };

        let mut client: OwnedRpcClient<PingMsg, PongMsg> =
            connect(rendezvous, 0, 0).await.expect("connect");
        let reply = client
            .request(PingMsg("ping".to_string()))
            .await
            .expect("request");
        assert_eq!(reply, PongMsg("pong".to_string()));

        server.await.expect("server task");
    }

    #[tokio::test]
    async fn server_stream_connect_accept_round_trip() {
        // connect_server_stream allocates the region pair and delivers the
        // shared_id via the rendezvous; accept_server_stream attaches the
        // same pair server-side. Items then flow in order to stream end.
        //
        // connect waits for the server's accept before returning (see
        // wait_for_server_accept), so the server task is spawned first and
        // retries the rendezvous until the connect-side send lands.
        setup();
        use futures::StreamExt;

        let rendezvous = crate::ShmRendezvous::new();

        let server = {
            let rendezvous = rendezvous.clone();
            tokio::spawn(async move {
                let incoming = loop {
                    match rendezvous.recv().await {
                        Ok(connection) => break connection,
                        Err(_) => tokio::task::yield_now().await,
                    }
                };
                let mut conn: ServerStreamConnection<PingMsg, PongMsg> =
                    accept_server_stream(incoming).expect("accept_server_stream");
                let mut req = conn.recv().await.expect("recv request");
                assert_eq!(req.payload().expect("decode request").0, "start");
                req.send_item(PongMsg("a".to_string()))
                    .await
                    .expect("send item");
                req.send_final_item(PongMsg("b".to_string()))
                    .await
                    .expect("send final item");
            })
        };

        let mut client: OwnedServerStreamClient<PingMsg, PongMsg> =
            connect_server_stream(rendezvous, 0, 0)
                .await
                .expect("connect_server_stream");
        assert!(client.shared_id() > 0, "session region must be allocated");

        let mut stream = client
            .call(PingMsg("start".to_string()))
            .await
            .expect("call");
        let mut items = Vec::new();
        while let Some(item) = stream.next().await {
            items.push(item.expect("stream item"));
        }
        assert_eq!(
            items,
            vec![PongMsg("a".to_string()), PongMsg("b".to_string())]
        );

        server.await.expect("server task");
    }
}
