//! Connector-side per-stream byte channel.
//!
//! A `QuicChannel` owns one two-ring shared-memory region: the connector
//! allocates it, delivers the parent `shared_id` to the serving guest, and
//! both peers attach their halves of the region as a [`ByteStream`]
//! (`AsyncRead`/`AsyncWrite`). The connector is the mirror of the guest's
//! stream: it reads the guest's outbound ring (guest → client) and writes the
//! guest's inbound ring (client → guest).
//!
//! The byte-channel framing is exactly the shared [`ByteStream`]
//! implementation used by `TcpStream`, so TCP and QUIC channels share one
//! layout and one code path.

use selium_guest::{
    GuestError, Result,
    net::bytes::{ByteStream, ByteStreamReader, ByteStreamWriter},
};
use selium_shm::byte_channel;

/// Default per-direction ring capacity for a QUIC stream channel.
pub const DEFAULT_STREAM_RING_CAPACITY: u64 = 64 * 1024;

/// A connector-owned byte channel over a freshly allocated two-ring region.
pub struct QuicChannel {
    reader: Option<ByteStreamReader>,
    writer: Option<ByteStreamWriter>,
    shared_id: u64,
}

impl QuicChannel {
    /// Allocates a fresh two-ring region and attaches this (connector) peer.
    pub fn allocate() -> Result<Self> {
        Self::allocate_for_tenant(None)
    }

    /// Allocates a fresh two-ring region minted under `serving_tenant` (the
    /// authenticated client's tenant): principal provenance so bridged stream
    /// regions land under the tenant they serve rather than the root
    /// connector's namespace.
    pub fn allocate_for_tenant(serving_tenant: Option<&str>) -> Result<Self> {
        Self::with_capacity_for_tenant(
            DEFAULT_STREAM_RING_CAPACITY,
            DEFAULT_STREAM_RING_CAPACITY,
            serving_tenant,
        )
    }

    /// Like [`allocate`](Self::allocate), with explicit per-direction ring
    /// capacities (client → guest, guest → client).
    pub fn with_capacity(client_to_guest: u64, guest_to_client: u64) -> Result<Self> {
        Self::with_capacity_for_tenant(client_to_guest, guest_to_client, None)
    }

    /// Like [`with_capacity`](Self::with_capacity), minting the region under
    /// `serving_tenant`.
    pub fn with_capacity_for_tenant(
        client_to_guest: u64,
        guest_to_client: u64,
        serving_tenant: Option<&str>,
    ) -> Result<Self> {
        // `create` allocates the region pair with this side already
        // attached (the allocation maps it), so build the halves from its
        // channels and region handle directly. A second `attach` on the
        // same region would be rejected by the runtime's region provider
        // ("already attached"); the mirror peer attaches via `shared_id`.
        let (ring_to_guest, ring_from_guest, shared_id, region) =
            byte_channel::create_for_tenant(client_to_guest, guest_to_client, serving_tenant)
                .map_err(|e| GuestError::Host(format!("allocate stream region: {e}")))?;

        // Connector mirrors the guest: read the guest's outbound ring and
        // write the guest's inbound ring. The blocking writer registers a
        // writer count so the guest's reader observes EOF when the connector
        // closes its write half.
        let (reader, writer) =
            ByteStream::halves_from_channels(&ring_from_guest, &ring_to_guest, region, true)?;

        Ok(Self {
            reader: Some(reader),
            writer: Some(writer),
            shared_id,
        })
    }

    /// Returns the parent region's shared id, to be delivered to the serving
    /// guest via its listener queue.
    pub fn shared_id(&self) -> u64 {
        self.shared_id
    }

    /// Returns the read half (guest → client).
    pub fn reader_mut(&mut self) -> &mut ByteStreamReader {
        self.reader.as_mut().expect("read half present")
    }

    /// Returns the write half (client → guest).
    pub fn writer_mut(&mut self) -> &mut ByteStreamWriter {
        self.writer.as_mut().expect("write half present")
    }

    /// Consumes the channel, yielding its independent read/write halves.
    ///
    /// Ownership of the region passes to the halves (and, on the peer side,
    /// to the guest's attach): the returned channel handle no longer
    /// reclaims it, so a live relay is never torn down from under its
    /// peers.
    pub fn into_halves(mut self) -> (ByteStreamReader, ByteStreamWriter) {
        let reader = self.reader.take().expect("read half present");
        let writer = self.writer.take().expect("write half present");
        // Mark the region as handed off so `Drop` does not free it while
        // both peers still hold mappings (the runtime refuses such a free
        // anyway; teardown reclaims whatever remains).
        self.shared_id = 0;
        (reader, writer)
    }
}

impl Drop for QuicChannel {
    fn drop(&mut self) {
        // Drop the read/write halves first (releasing writer slots and
        // decrementing the writer count, which surfaces EOF to the peer)
        // before considering reclaiming the shared region.
        drop(self.reader.take());
        drop(self.writer.take());
        // Reclaim the region pair only when this handle still owns it
        // (whole-channel drop, e.g. failed delivery — the peer never
        // attached). Best-effort: the region may already have been
        // reclaimed by a peer.
        if self.shared_id != 0 {
            drop(selium_shm::free_region(self.shared_id));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use selium_guest::net::ByteStream;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn setup() {
        drop(selium_memory::set_region_provider(Box::new(
            selium_memory::HeapRegionProvider::new(),
        )));
    }

    #[tokio::test]
    async fn connector_and_guest_halves_round_trip_bytes() {
        setup();

        // The connector allocates the channel and delivers `shared_id` to the
        // guest side, which attaches its mirror half.
        let mut connector = QuicChannel::allocate().expect("allocate channel");
        let shared_id = connector.shared_id();
        let mut guest = ByteStream::attach_blocking(shared_id).expect("guest attach");

        // Connector → guest: client bytes relayed onto the guest's inbound ring.
        connector
            .writer_mut()
            .write_all(b"hello guest")
            .await
            .expect("connector write");
        let mut buf = [0u8; 11];
        guest.read_exact(&mut buf).await.expect("guest read");
        assert_eq!(&buf, b"hello guest");

        // Guest → connector: guest bytes relayed out of its outbound ring.
        guest.write_all(b"hello client").await.expect("guest write");
        let mut buf = [0u8; 12];
        connector
            .reader_mut()
            .read_exact(&mut buf)
            .await
            .expect("connector read");
        assert_eq!(&buf, b"hello client");
    }

    #[tokio::test]
    async fn connector_close_surfaces_eof_to_guest() {
        setup();

        let mut connector = QuicChannel::allocate().expect("allocate channel");
        let shared_id = connector.shared_id();
        let mut guest = ByteStream::attach_blocking(shared_id).expect("guest attach");

        // Client data relayed, then the connector closes its write half
        // (client FIN): the guest must observe the data, then EOF.
        connector
            .writer_mut()
            .write_all(b"data")
            .await
            .expect("connector write");
        drop(connector);

        let mut buf = [0u8; 4];
        guest.read_exact(&mut buf).await.expect("guest read");
        assert_eq!(&buf, b"data");

        let mut probe = [0u8; 1];
        let n = guest.read(&mut probe).await.expect("guest eof read");
        assert_eq!(n, 0, "connector close must surface EOF to the guest");
    }
}
