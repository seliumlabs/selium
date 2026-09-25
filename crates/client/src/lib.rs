//! Native Selium client SDK.
//!
//! `selium-client` collapses the QUIC endpoint, TLS/mTLS configuration, bridge
//! handshake, and `selium-wire` framing into a single host-side crate: call
//! [`connect`], then open typed channel handles over one connection.
//!
//! The bridge handshake is deterministic: every channel open sends the typed
//! handshake and awaits the bridge's control reply, so attach failures surface
//! as typed errors at open ([`Error::AttachFailed`],
//! [`Error::BadHandshake`]), not on first use. Message types are supplied by
//! the caller as Flatbuffers-backed [`FlatMsg`] values (re-exported here
//! alongside the `selium-service` crate), and every handle surfaces this
//! crate's [`Error`] type.

use std::{
    net::SocketAddr,
    pin::Pin,
    task::{Context, Poll},
};

use futures::{Sink, Stream};
use selium_wire::{
    PipeControl,
    framed::{FramedRead, FramedWrite},
};

pub use crate::error::{Error, Result};
// Re-export the encoding surface so users import message bindings from the
// client alone (see the "Encoding Re-exports" requirement).
pub use selium_service;
pub use selium_service::FlatMsg;
pub use tls::{
    ClientIdentity, ConnectOptions, build_client_config, certificates_from_pem,
    private_key_from_pem,
};
pub use transport::QuicTransport;

pub mod error;
pub mod tls;
pub mod transport;

/// A typed subscriber, yielding decoded messages as a [`Stream`].
///
/// Thin newtype over `selium_wire::Subscriber` that maps every item error
/// onto the crate error type.
pub struct Subscriber<T> {
    inner: selium_wire::Subscriber<T, QuicTransport>,
}

/// A typed publisher, writing encoded messages as frames to its channel.
///
/// Implements [`Sink`] with the crate error type and also offers an
/// infallible-await [`publish`](Self::publish) for simple senders. Distinct
/// publishers on one topic SHOULD take distinct
/// [`writer_id`](Self::set_writer_id)s so live-table replay can tell them
/// apart.
pub struct Publisher<T> {
    inner: selium_wire::Publisher<T, QuicTransport>,
}

/// A typed request/response RPC client over its channel.
pub struct RpcClient<Req, Rep> {
    inner: selium_wire::RpcClient<Req, Rep, QuicTransport>,
}

/// A typed server-streaming RPC client over its channel.
pub struct RpcServerStreamClient<Req, Item> {
    inner: selium_wire::RpcServerStreamClient<Req, Item, QuicTransport>,
}

/// A live server-stream received in response to a request.
pub struct RpcServerStream<'a, Item> {
    inner: selium_wire::RpcServerStream<'a, Item, QuicTransport>,
}

/// A typed bidirectional-streaming RPC client over its channel.
pub struct RpcBidiStreamClient<Req, Item, Resp> {
    inner: selium_wire::RpcBidiStreamClient<Req, Item, Resp, QuicTransport>,
}

/// An established bidirectional-streaming session on the client side.
pub struct RpcBidiStream<'a, Item, Resp> {
    inner: selium_wire::RpcBidiStream<'a, Item, Resp, QuicTransport>,
}

/// The send half of a bidi session, obtained via [`RpcBidiStream::split`].
pub struct BidiSender<'a, Item> {
    inner: selium_wire::BidiSender<'a, Item, QuicTransport>,
}

/// The receive half of a bidi session, obtained via [`RpcBidiStream::split`].
pub struct BidiReceiver<'a, Resp> {
    inner: selium_wire::BidiReceiver<'a, Resp, QuicTransport>,
}

/// A live table projected from a pub/sub topic.
pub struct LiveTable<K, V> {
    inner: selium_wire::LiveTable<K, V, QuicTransport>,
}

/// A connected Selium client: one QUIC connection plus the handles to open
/// channels on it.
pub struct Client {
    // Owned so the connection stays alive until the client is dropped.
    _endpoint: quinn::Endpoint,
    connection: quinn::Connection,
}

impl<T> Subscriber<T> {
    fn new(reader: FramedRead<QuicTransport>) -> Self {
        Self {
            inner: selium_wire::Subscriber::new(reader, None),
        }
    }
}

impl<T: FlatMsg + Unpin> Stream for Subscriber<T> {
    type Item = Result<T>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Pin::new(&mut self.get_mut().inner)
            .poll_next(cx)
            .map(|item| item.map(|result| result.map_err(Error::from)))
    }
}

impl<T: FlatMsg> Publisher<T> {
    /// Sets this publisher's correlation tag (writer id).
    pub fn set_writer_id(&mut self, writer_id: u32) {
        self.inner.set_writer_id(writer_id);
    }

    /// Returns this publisher's correlation tag (writer id).
    pub fn writer_id(&self) -> u32 {
        self.inner.writer_id()
    }

    /// Encodes and writes one message to the channel.
    pub fn publish(&mut self, message: &T) -> Result<()> {
        self.inner.publish(message).map_err(Error::from)
    }
}

impl<T: FlatMsg + Unpin> Sink<T> for Publisher<T> {
    type Error = Error;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<()>> {
        Pin::new(&mut self.get_mut().inner)
            .poll_ready(cx)
            .map(|poll| poll.map_err(Error::from))
    }

    fn start_send(self: Pin<&mut Self>, item: T) -> Result<()> {
        Pin::new(&mut self.get_mut().inner)
            .start_send(item)
            .map_err(Error::from)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<()>> {
        Pin::new(&mut self.get_mut().inner)
            .poll_flush(cx)
            .map(|poll| poll.map_err(Error::from))
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<()>> {
        Pin::new(&mut self.get_mut().inner)
            .poll_close(cx)
            .map(|poll| poll.map_err(Error::from))
    }
}

impl<Req: FlatMsg, Rep: FlatMsg> RpcClient<Req, Rep> {
    /// Sends one request and awaits its correlated reply.
    pub async fn request(&mut self, payload: Req) -> Result<Rep> {
        self.inner.request(payload).await.map_err(Error::from)
    }
}

impl<Req: FlatMsg, Item: FlatMsg> RpcServerStreamClient<Req, Item> {
    /// Sends one request and returns a [`Stream`] of reply items.
    pub async fn call(&mut self, req: Req) -> Result<RpcServerStream<'_, Item>> {
        self.inner
            .call(req)
            .await
            .map(|inner| RpcServerStream { inner })
            .map_err(Error::from)
    }
}

impl<Item: FlatMsg + Unpin> RpcServerStream<'_, Item> {
    /// Sends a cancel frame to the peer and ends the stream.
    pub fn cancel(&mut self) {
        self.inner.cancel();
    }
}

impl<Item: FlatMsg + Unpin> Stream for RpcServerStream<'_, Item> {
    type Item = Result<Item>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Pin::new(&mut self.get_mut().inner)
            .poll_next(cx)
            .map(|item| item.map(|result| result.map_err(Error::from)))
    }
}

impl<Req: FlatMsg, Item: FlatMsg, Resp: FlatMsg> RpcBidiStreamClient<Req, Item, Resp> {
    /// Sends the opening request and returns the bidi session handle.
    pub async fn connect(&mut self, req: Req) -> Result<RpcBidiStream<'_, Item, Resp>> {
        self.inner
            .connect(req)
            .await
            .map(|inner| RpcBidiStream { inner })
            .map_err(Error::from)
    }
}

impl<Item, Resp> RpcBidiStream<'_, Item, Resp> {
    /// Returns the correlation tag for this session.
    pub fn correlation(&self) -> u32 {
        self.inner.correlation()
    }

    /// Splits the session into independent send and receive halves.
    pub fn split(&mut self) -> (BidiSender<'_, Item>, BidiReceiver<'_, Resp>) {
        let (sender, receiver) = self.inner.split();
        (
            BidiSender { inner: sender },
            BidiReceiver { inner: receiver },
        )
    }
}

impl<Item: FlatMsg> BidiSender<'_, Item> {
    /// Sends one item to the peer.
    pub async fn send(&mut self, item: Item) -> Result<()> {
        self.inner.send(item).await.map_err(Error::from)
    }

    /// Sends one item and closes the sending direction.
    pub async fn close_with_item(&mut self, item: Item) -> Result<()> {
        self.inner.close_with_item(item).await.map_err(Error::from)
    }

    /// Closes the sending direction.
    pub async fn close(&mut self) -> Result<()> {
        self.inner.close().await.map_err(Error::from)
    }

    /// Returns `true` once the sending direction is closed.
    pub fn is_closed(&self) -> bool {
        self.inner.is_closed()
    }
}

impl<Resp: FlatMsg> BidiReceiver<'_, Resp> {
    /// Returns `true` once the receiving direction has ended.
    pub fn is_closed(&self) -> bool {
        self.inner.is_closed()
    }

    /// Receives the next reply item, or `None` once the stream ended.
    pub async fn recv(&mut self) -> Result<Option<Resp>> {
        self.inner.recv().await.map_err(Error::from)
    }

    /// Non-blocking receive of a reply item if one is immediately available.
    pub fn try_recv(&mut self) -> Result<Option<Resp>> {
        self.inner.try_recv().map_err(Error::from)
    }
}

impl<K, V> LiveTable<K, V>
where
    K: FlatMsg + Clone + Eq + std::hash::Hash,
    V: FlatMsg + Clone,
{
    /// Inserts or updates a value, applying buffered remote mutations first.
    pub fn set(&self, key: K, value: V) -> Result<()> {
        self.inner.set(key, value).map_err(Error::from)
    }

    /// Insert or update with compare-and-set version checking.
    pub fn compare_and_set(&self, key: K, expected_version: u64, value: V) -> Result<u64> {
        self.inner
            .compare_and_set(key, expected_version, value)
            .map_err(Error::from)
    }

    /// Deletes a value, applying buffered remote mutations first.
    pub fn delete(&self, key: K) -> Result<()> {
        self.inner.delete(key).map_err(Error::from)
    }

    /// Returns the current value for a key, applying buffered remote
    /// mutations first.
    pub fn get(&self, key: &K) -> Result<Option<V>> {
        self.inner.get(key).map_err(Error::from)
    }

    /// Returns the full record (value + version) for a key.
    pub fn get_record(&self, key: &K) -> Result<Option<selium_wire::LiveTableRecord<V>>> {
        self.inner.get_record(key).map_err(Error::from)
    }

    /// Returns the record version for a key.
    pub fn get_version(&self, key: &K) -> Result<Option<u64>> {
        self.inner.get_version(key).map_err(Error::from)
    }

    /// Returns up to `limit` live records.
    pub fn scan(&self, limit: usize) -> Result<Vec<(K, selium_wire::LiveTableRecord<V>)>> {
        self.inner.scan(limit).map_err(Error::from)
    }

    /// Drains buffered remote mutations into the local view.
    pub fn sync(&self) -> Result<()> {
        self.inner.sync().map_err(Error::from)
    }

    /// Drains the subscriber, then parks on the transport's read waker until
    /// the next remote mutation arrives.
    pub async fn sync_async(&self) -> Result<()> {
        self.inner.sync_async().await.map_err(Error::from)
    }

    /// Inserts or updates a value, then parks until the table's own mutation
    /// is replayed back to it.
    pub async fn set_async(&self, key: K, value: V) -> Result<()> {
        self.inner.set_async(key, value).await.map_err(Error::from)
    }

    /// Deletes a value, then parks until the deletion is replayed back.
    pub async fn delete_async(&self, key: K) -> Result<()> {
        self.inner.delete_async(key).await.map_err(Error::from)
    }
}

impl Client {
    /// Returns the underlying QUIC connection (for raw-stream use cases).
    pub fn connection(&self) -> &quinn::Connection {
        &self.connection
    }

    /// Writes the handshake frame on the raw send half, returning the half.
    fn write_handshake(send: quinn::SendStream, uri: &str) -> Result<quinn::SendStream> {
        let mut framed = FramedWrite::new(QuicTransport::write_only(send));
        let handshake = PipeControl::Handshake {
            uri: uri.to_string(),
        };
        let handshake = FlatMsg::encode(&handshake);
        framed.write_frame(&handshake, 0)?;
        let (send, _) = framed.into_inner().into_parts();
        send.ok_or_else(|| Error::Io(std::io::Error::other("send half missing")))
    }

    /// Awaits the deterministic bridge reply on `reader`, returning the
    /// reader with its buffered read-ahead intact.
    ///
    /// The reply is read through the same framed reader the handle keeps:
    /// the codec may read past the reply into relayed data frames, and
    /// rebuilding the reader would silently discard them.
    async fn await_acceptance(
        mut reader: FramedRead<QuicTransport>,
    ) -> Result<FramedRead<QuicTransport>> {
        let (payload, _tag, _flags) = match reader.read_frame_async().await {
            Ok(frame) => frame,
            Err(selium_wire::Error::Terminated) => {
                // The bridge closed without replying: a protocol violation.
                return Err(Error::BadHandshake);
            }
            Err(e) => return Err(Error::from(e)),
        };
        match FlatMsg::decode(&payload) {
            Ok(PipeControl::Accepted) => Ok(reader),
            Ok(PipeControl::Terminate { code }) => Err(crate::error::termination_error(code)),
            // Anything else (or an undecodable reply) violates the contract.
            _ => Err(Error::BadHandshake),
        }
    }

    /// Opens a fresh bidirectional stream, performs the deterministic
    /// handshake, and returns the raw send half plus the reply reader (with
    /// its read-ahead intact) for handles that own their read path.
    async fn open_split(
        &self,
        uri: &str,
    ) -> Result<(quinn::SendStream, FramedRead<QuicTransport>)> {
        let (send, recv) = self.connection.open_bi().await?;
        let send = Self::write_handshake(send, uri)?;
        let reader = FramedRead::new(QuicTransport::read_only(recv));
        let reader = Self::await_acceptance(reader).await?;
        Ok((send, reader))
    }

    /// Opens a typed publisher channel for `uri`.
    pub async fn publisher<T: FlatMsg>(&self, uri: &str) -> Result<Publisher<T>> {
        let (send, reader) = self.open_split(uri).await?;
        // The publisher never reads, so the reply reader's read-ahead
        // (relayed topic data) may be dropped with it; both halves must live
        // in one transport so neither direction FINs the bridge relay.
        let (_, recv) = reader.into_inner().into_parts();
        let recv = recv.ok_or_else(|| Error::Io(std::io::Error::other("recv half missing")))?;
        Ok(Publisher {
            inner: selium_wire::Publisher::new(FramedWrite::new(QuicTransport::bi(send, recv))),
        })
    }

    /// Opens a typed subscriber channel for `uri`.
    pub async fn subscriber<T: FlatMsg>(&self, uri: &str) -> Result<Subscriber<T>> {
        let (send, recv) = self.connection.open_bi().await?;
        let send = Self::write_handshake(send, uri)?;
        // Both halves live in one transport so the subscriber (which never
        // writes) does not FIN the bridge relay.
        let reader = FramedRead::new(QuicTransport::bi(send, recv));
        let reader = Self::await_acceptance(reader).await?;
        Ok(Subscriber::new(reader))
    }

    /// Opens a typed request/response RPC channel for `uri`.
    pub async fn rpc<Req: FlatMsg, Rep: FlatMsg>(&self, uri: &str) -> Result<RpcClient<Req, Rep>> {
        let (send, reader) = self.open_split(uri).await?;
        Ok(RpcClient {
            inner: selium_wire::RpcClient::new(
                FramedWrite::new(QuicTransport::write_only(send)),
                reader,
            ),
        })
    }

    /// Opens a typed server-streaming RPC channel for `uri`.
    pub async fn server_stream<Req: FlatMsg, Item: FlatMsg>(
        &self,
        uri: &str,
    ) -> Result<RpcServerStreamClient<Req, Item>> {
        let (send, reader) = self.open_split(uri).await?;
        Ok(RpcServerStreamClient {
            inner: selium_wire::RpcServerStreamClient::new(
                FramedWrite::new(QuicTransport::write_only(send)),
                reader,
            ),
        })
    }

    /// Opens a typed bidirectional-streaming RPC channel for `uri`.
    pub async fn bidi_stream<Req: FlatMsg, Item: FlatMsg, Resp: FlatMsg>(
        &self,
        uri: &str,
    ) -> Result<RpcBidiStreamClient<Req, Item, Resp>> {
        let (send, reader) = self.open_split(uri).await?;
        Ok(RpcBidiStreamClient {
            inner: selium_wire::RpcBidiStreamClient::new(
                FramedWrite::new(QuicTransport::write_only(send)),
                reader,
            ),
        })
    }

    /// Opens a typed live-table channel for `uri`.
    pub async fn live_table<K, V>(&self, uri: &str) -> Result<LiveTable<K, V>>
    where
        K: FlatMsg + Clone + Eq + std::hash::Hash,
        V: FlatMsg + Clone,
    {
        let (send, reader) = self.open_split(uri).await?;

        let publisher: selium_wire::Publisher<selium_wire::LiveTableMessage<K, V>, QuicTransport> =
            selium_wire::Publisher::new(FramedWrite::new(QuicTransport::write_only(send)));
        let subscriber: selium_wire::Subscriber<
            selium_wire::LiveTableMessage<K, V>,
            QuicTransport,
        > = selium_wire::Subscriber::new(reader, None);

        Ok(LiveTable {
            inner: selium_wire::LiveTable::new(publisher, subscriber)?,
        })
    }
}

/// Establishes a single QUIC (TLS 1.3) connection to `addr`, trusting the
/// server certificate supplied in `options`, and returns a [`Client`].
pub async fn connect(addr: SocketAddr, options: ConnectOptions) -> Result<Client> {
    let bind = SocketAddr::from(([0, 0, 0, 0], 0));
    let mut endpoint = quinn::Endpoint::client(bind)?;
    endpoint.set_default_client_config(build_client_config(&options)?);

    let connection = endpoint.connect(addr, &options.server_name)?.await?;
    Ok(Client {
        _endpoint: endpoint,
        connection,
    })
}
