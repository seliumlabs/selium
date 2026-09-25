use selium_abi::ResourceKind;
use selium_wire::error::Result;

use crate::ring_buf::{RingBuf, round_capacity};

pub use self::{
    reader::{BlockingReader, HasGeneration, Reader, WeakReader},
    writer::{BlockingWriter, Writer},
};

pub mod reader;
pub mod writer;

/// Backpressure strategy for channel writers.
///
/// Determines how writers respond to slow blocking readers and slow blocking writers:
/// - `Park`: writers block (return Pending) when the ring is full
/// - `Drop`: writers silently drop data (return Ok without writing) when the ring is full
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelBackpressure {
    /// Writers respect blocking reader and writer positions; writes block when consumers
    /// or other blocking writers fall behind.
    Park,
    /// Writers never block; slow blocking consumers or blocking writers cause data to be
    /// silently dropped.
    Drop,
}

/// A shared-memory-backed channel with [non-]blocking reader and writer semantics.
///
/// The channel stores framed messages in a lock-free ring buffer. Blocking readers
/// prevent buffer overwrite until they have consumed data; non-blocking readers may lose
/// data if they fall behind. Blocking writers register a writer slot that prevents
/// other writers from overwriting their data; non-blocking writers may be starved.
///
/// # Backpressure Strategies
///
/// ## Park (default)
/// - Slow blocking reader → all writers backpressure (block)
/// - Slow blocking writer → all other writers backpressure (round-robin)
/// - Slow non-blocking reader → loses data
/// - Slow non-blocking writer → may be starved by other writers
///
/// ## Drop
/// - Slow blocking reader → all writers silently drop data
/// - Slow blocking writer → all other writers silently drop data
/// - Slow non-blocking reader → loses data
/// - Slow non-blocking writer → may be starved by other writers
///
/// Notification uses the generation counter in the shared region with native
/// atomic wait/notify instead of signals.
pub struct Channel {
    ring: RingBuf,
    backpressure: ChannelBackpressure,
}

impl ChannelBackpressure {
    /// Converts to the shared-memory wire format (0 = Park, 1 = Drop).
    pub fn to_u8(self) -> u8 {
        match self {
            Self::Park => 0,
            Self::Drop => 1,
        }
    }

    /// Converts from the shared-memory wire format.
    ///
    /// Unrecognised values default to `Park`.
    pub fn from_u8(value: u8) -> Self {
        match value {
            1 => Self::Drop,
            _ => Self::Park,
        }
    }
}

impl Channel {
    /// Creates a new channel with the given ring buffer data capacity and backpressure strategy.
    ///
    /// Uses `ResourceKind::SharedMemory` as the default purpose. For channels with
    /// a specific purpose (e.g., log channels, RPC rings), use
    /// [`Channel::create_with_backpressure`] instead.
    pub fn create(capacity: u64, backpressure: ChannelBackpressure) -> Result<Self> {
        Self::create_with_backpressure(capacity, backpressure, ResourceKind::SharedMemory)
    }

    /// Creates a new channel with the given ring buffer data capacity, backpressure
    /// strategy, and resource purpose.
    ///
    /// The `purpose` is threaded through to the underlying `AllocRegion` hostcall
    /// for runtime discovery registration.
    pub fn create_with_backpressure(
        capacity: u64,
        backpressure: ChannelBackpressure,
        purpose: ResourceKind,
    ) -> Result<Self> {
        let capacity = round_capacity(capacity)?;
        let ring = RingBuf::create(capacity, purpose)?;
        ring.region().store_backpressure(backpressure.to_u8())?;
        Ok(Self { ring, backpressure })
    }

    /// Attaches to an existing channel by shared region id.
    ///
    /// Reads the backpressure strategy and capacity from the shared channel header.
    pub fn attach(region_id: u64) -> Result<Self> {
        let ring = RingBuf::attach(region_id)?;
        let backpressure = ChannelBackpressure::from_u8(ring.region().load_backpressure()?);
        Ok(Self { ring, backpressure })
    }

    /// Creates a blocking writer for this channel.
    ///
    /// The blocking writer registers a writer slot in shared memory. Other writers
    /// cannot advance past the blocking writer's position, preventing a single
    /// busy writer from starving the blocking writer. On Park channels this causes
    /// backpressure; on Drop channels other writers silently drop data.
    pub fn blocking_writer(&self) -> Result<BlockingWriter> {
        BlockingWriter::new(self.ring.region().clone(), self.backpressure)
    }

    /// Creates a non-blocking writer that does not contribute to `writer_count`.
    pub fn writer(&self) -> Result<Writer> {
        let writer_id = self.ring.region().allocate_writer_id()?;
        Ok(Writer::new(
            self.ring.region().clone(),
            writer_id,
            self.backpressure,
        ))
    }

    /// Creates a non-blocking reader that may lose data if slow.
    pub fn reader(&self) -> Reader {
        let tail = self.ring.read_next_tail().unwrap_or(0);
        Reader::new(self.ring.region().clone(), tail, self.backpressure)
    }

    /// Creates a blocking reader that prevents buffer overwrite.
    ///
    /// On Park channels, writers backpressure when the blocking reader is slow.
    /// On Drop channels, writers silently drop data when the blocking reader is slow.
    pub fn blocking_reader(&self) -> Result<BlockingReader> {
        let tail = self.ring.read_next_tail()?;
        let reader_id = self.ring.region().allocate_reader_slot(tail)?;
        Ok(BlockingReader::new(
            self.ring.region().clone(),
            tail,
            reader_id,
        ))
    }

    /// Returns a blocking reader starting at `start_pos`.
    ///
    /// Unlike [`blocking_reader`](Self::blocking_reader), which starts at
    /// the live tail (skipping frames written before the call), a reader
    /// from position `0` consumes everything in the ring — required for
    /// peer-to-peer byte channels, where the writing peer may have relayed
    /// frames before this peer attached.
    pub fn blocking_reader_from(&self, start_pos: u64) -> Result<BlockingReader> {
        let reader_id = self.ring.region().allocate_reader_slot(start_pos)?;
        Ok(BlockingReader::new(
            self.ring.region().clone(),
            start_pos,
            reader_id,
        ))
    }

    /// Creates a weak reader for this channel, starting at position `0`.
    ///
    /// Weak readers hold no reader slot, so they never backpressure writers:
    /// a full ring overwrites the oldest unread data while the newest always
    /// survives. Starting at `0`, the reader drains everything still present
    /// in the ring, snapping past any records overwritten before the first
    /// drain. Use for log drains and other fire-and-forget consumers.
    pub fn weak_reader(&self) -> WeakReader {
        WeakReader::new(self.ring.region().clone(), 0)
    }

    /// Returns the backpressure strategy for this channel.
    pub fn backpressure(&self) -> ChannelBackpressure {
        self.backpressure
    }

    /// Returns the underlying ring buffer.
    pub fn ring(&self) -> &RingBuf {
        &self.ring
    }

    /// Returns the shared region id.
    pub fn region_id(&self) -> u64 {
        self.ring.region_id()
    }

    /// Wraps an existing ring buffer as a channel.
    ///
    /// Used by higher-level patterns (e.g. RPC) that allocate their own ring
    /// regions and need a `Channel` handle for blocking readers/writers.
    pub fn from_ring(ring: RingBuf, backpressure: ChannelBackpressure) -> Self {
        Self { ring, backpressure }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drop_channel_accepts_blocking_writer() {
        let channel = Channel::create(64, ChannelBackpressure::Drop).expect("create");
        let result = channel.blocking_writer();
        result.unwrap();
    }

    #[test]
    fn park_channel_accepts_blocking_writer() {
        let channel = Channel::create(64, ChannelBackpressure::Park).expect("create");
        let result = channel.blocking_writer();
        result.unwrap();
    }

    #[test]
    fn drop_channel_accepts_blocking_reader() {
        let channel = Channel::create(64, ChannelBackpressure::Drop).expect("create");
        let result = channel.blocking_reader();
        result.unwrap();
    }

    #[test]
    fn park_channel_accepts_blocking_reader() {
        let channel = Channel::create(64, ChannelBackpressure::Park).expect("create");
        let result = channel.blocking_reader();
        result.unwrap();
    }

    #[test]
    fn drop_channel_writer_never_blocks() {
        let channel = Channel::create(64, ChannelBackpressure::Drop).expect("create");
        let writer = channel.writer().expect("writer");
        assert_eq!(writer.backpressure(), ChannelBackpressure::Drop);
    }

    #[test]
    fn park_channel_writer_uses_park_backpressure() {
        let channel = Channel::create(64, ChannelBackpressure::Park).expect("create");
        let writer = channel.writer().expect("writer");
        assert_eq!(writer.backpressure(), ChannelBackpressure::Park);
    }

    #[test]
    fn create_with_backpressure_is_alias() {
        let channel = Channel::create_with_backpressure(
            64,
            ChannelBackpressure::Drop,
            ResourceKind::LogChannel,
        )
        .expect("create");
        assert_eq!(channel.backpressure(), ChannelBackpressure::Drop);
    }

    #[test]
    fn backpressure_wire_format_round_trip() {
        assert_eq!(ChannelBackpressure::Park.to_u8(), 0);
        assert_eq!(ChannelBackpressure::Drop.to_u8(), 1);
        assert_eq!(ChannelBackpressure::from_u8(0), ChannelBackpressure::Park);
        assert_eq!(ChannelBackpressure::from_u8(1), ChannelBackpressure::Drop);
        // Unknown values default to Park.
        assert_eq!(ChannelBackpressure::from_u8(255), ChannelBackpressure::Park);
    }

    #[test]
    fn create_writes_backpressure_to_shared_header() {
        let channel = Channel::create(64, ChannelBackpressure::Drop).expect("create");
        let bp = channel.ring().region().load_backpressure().expect("load");
        assert_eq!(bp, ChannelBackpressure::Drop.to_u8());
    }

    #[test]
    fn create_writes_capacity_to_shared_header() {
        let channel = Channel::create(128, ChannelBackpressure::Park).expect("create");
        let cap = channel
            .ring()
            .region()
            .load_shared_capacity()
            .expect("load");
        assert_eq!(cap, 128);
    }

    #[test]
    fn attach_reads_backpressure_from_shared_header() {
        // Create a Drop channel — backpressure and capacity are written to the
        // shared header.
        let channel = Channel::create(64, ChannelBackpressure::Drop).expect("create");
        let region_id = channel.region_id();

        // Verify the shared header contains the wire-format backpressure.
        assert_eq!(
            channel.ring().region().load_backpressure().expect("load"),
            1
        );

        // Attach by region_id — should read backpressure from the shared header.
        let attached = Channel::attach(region_id).expect("attach");
        assert_eq!(attached.backpressure(), ChannelBackpressure::Drop);
        assert_eq!(attached.ring().capacity(), 64);

        drop(crate::free_region(region_id));
    }

    #[test]
    fn attach_park_channel_from_shared_header() {
        let channel = Channel::create(128, ChannelBackpressure::Park).expect("create");
        let region_id = channel.region_id();

        let attached = Channel::attach(region_id).expect("attach");
        assert_eq!(attached.backpressure(), ChannelBackpressure::Park);
        assert_eq!(attached.ring().capacity(), 128);

        drop(crate::free_region(region_id));
    }
}
