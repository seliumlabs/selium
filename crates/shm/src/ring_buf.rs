use selium_abi::ResourceKind;
use selium_memory::FrameHeader;
use selium_wire::error::{Error, Result};

use crate::{
    ChannelRegion,
    layout::{Cursor, mask_for_capacity},
};

/// Minimum ring buffer data capacity (in bytes).
/// Must hold at least one 16-byte frame header plus a small payload.
const MIN_RING_CAPACITY: u64 = 64;

/// A lock-free ring buffer log on shared memory.
///
/// This is the primitive building block for all selium-io patterns.
/// It stores framed messages sequentially in a shared memory region.
/// Writers use a single-phase write protocol with release/acquire fencing:
/// payload is written first, then a release fence, then the header with the
/// READY flag. Readers use an acquire fence before reading the header.
///
/// Cross-process notification uses the generation counter in the shared region
/// with `memory.atomic.wait32` / `memory.atomic.notify` instead of signals.
pub struct RingBuf {
    region: ChannelRegion,
    mask: u64,
    capacity: u64,
}

impl RingBuf {
    /// Creates a new ring buffer with the given data capacity, backed by a fresh shared memory region.
    ///
    /// The `purpose` tag is threaded through to the `AllocRegion` hostcall for
    /// runtime discovery registration (informational only, not used for AAA).
    pub fn create(capacity: u64, purpose: ResourceKind) -> Result<Self> {
        #[cfg(test)]
        crate::ensure_heap_provider();
        let provider = selium_memory::region_provider()?;
        Self::create_with_provider(provider, capacity, purpose)
    }

    /// Creates a ring buffer through an explicit region provider.
    pub fn create_with_provider(
        provider: &dyn selium_memory::RegionProvider,
        capacity: u64,
        purpose: ResourceKind,
    ) -> Result<Self> {
        let region = ChannelRegion::create_with_provider(provider, capacity, purpose)?;
        let mask = mask_for_capacity(capacity)?;
        region.initialise()?;
        region.store_shared_capacity(capacity)?;
        Ok(Self {
            region,
            mask,
            capacity,
        })
    }

    /// Attaches to an existing ring buffer by shared region id.
    ///
    /// Reads the capacity from the shared channel header.
    pub fn attach(region_id: u64) -> Result<Self> {
        #[cfg(test)]
        crate::ensure_heap_provider();
        let provider = selium_memory::region_provider()?;
        Self::attach_with_provider(provider, region_id)
    }

    /// Attaches a ring buffer through an explicit region provider.
    pub fn attach_with_provider(
        provider: &dyn selium_memory::RegionProvider,
        region_id: u64,
    ) -> Result<Self> {
        let region = ChannelRegion::attach_with_provider(provider, region_id)?;
        Self::wrap_region(region)
    }

    /// Wraps an existing channel region as a ring buffer.
    pub fn wrap_region(region: ChannelRegion) -> Result<Self> {
        let capacity = region.capacity();
        let mask = mask_for_capacity(capacity)?;
        Ok(Self {
            region,
            mask,
            capacity,
        })
    }

    /// Returns a reference to the underlying region.
    pub fn region(&self) -> &ChannelRegion {
        &self.region
    }

    /// Returns the ring buffer capacity in bytes.
    pub fn capacity(&self) -> u64 {
        self.capacity
    }

    /// Returns the mask for wrapping positions.
    pub fn mask(&self) -> u64 {
        self.mask
    }

    /// Returns the shared region id.
    pub fn region_id(&self) -> u64 {
        self.region.region_id()
    }

    /// Reads the current next_tail from private state.
    pub fn read_next_tail(&self) -> Result<u64> {
        self.region.read_next_tail()
    }

    /// Reads the tail_cache from private state.
    pub fn read_tail_cache(&self) -> Result<u64> {
        self.region.read_tail_cache()
    }

    /// Atomically reserves space at the write tail. Returns the start position.
    ///
    /// Protects readers from overwrite but does not check writer slots (the
    /// ring buffer is a lower-level abstraction that does not manage writer slots).
    pub fn reserve(&self, len: u64) -> Result<u64> {
        self.region.reserve_tail(len, true, false)
    }

    /// Writes data at a logical position, handling wraparound.
    pub fn write_at(&self, pos: u64, data: &[u8]) -> Result<()> {
        let cursor = Cursor::new(pos);
        let (tail_len, head_len) = cursor.split_wraparound(data.len() as u64, self.mask);
        let raw_pos = cursor.masked(self.mask);
        if tail_len > 0 {
            self.region
                .write_data(raw_pos, data.get(..tail_len as usize).unwrap_or_default())?;
        }
        if head_len > 0 {
            self.region
                .write_data(0, data.get(tail_len as usize..).unwrap_or_default())?;
        }
        Ok(())
    }

    /// Reads up to `len` bytes from a logical position, handling wraparound.
    pub fn read_at(&self, pos: u64, len: u64) -> Result<Vec<u8>> {
        let cursor = Cursor::new(pos);
        let (tail_len, head_len) = cursor.split_wraparound(len, self.mask);
        let raw_pos = cursor.masked(self.mask);
        let mut result = Vec::with_capacity(len as usize);
        if tail_len > 0 {
            let data = self.region.read_data(raw_pos, tail_len)?;
            result.extend_from_slice(&data);
        }
        if head_len > 0 {
            let data = self.region.read_data(0, head_len)?;
            result.extend_from_slice(&data);
        }
        Ok(result)
    }

    /// Writes a framed message using single-phase write with release fencing.
    ///
    /// 1. Write placeholder header (checksum-valid, not READY) at `pos`.
    /// 2. Write payload at `pos + ENCODED_SIZE`
    /// 3. Release fence
    /// 4. Write header with READY flag at `pos`
    /// 5. Bump generation counter and notify waiters
    pub fn write_frame(&self, pos: u64, payload: &[u8], tag: u32, flags: u8) -> Result<()> {
        self.region.write_frame_at(pos, payload, tag, flags)
    }

    /// Reads a frame header from a position with acquire fencing.
    pub fn read_frame_header(&self, pos: u64) -> Result<FrameHeader> {
        crate::layout::read_frame_header(
            self.region.mapping().backend(),
            pos,
            self.mask,
            self.capacity,
        )
    }

    /// Reads a full framed message from a position with acquire fencing.
    pub fn read_frame(&self, pos: u64) -> Result<(FrameHeader, Vec<u8>)> {
        crate::layout::read_frame(
            self.region.mapping().backend(),
            pos,
            self.mask,
            self.capacity,
        )?
        .ok_or(Error::BufferEmpty)
    }

    /// Returns the current generation counter value.
    pub fn generation(&self) -> Result<u64> {
        self.region.load_generation()
    }
}

/// Rounds a byte capacity to the next power of two for use as a ring buffer.
pub fn round_capacity(capacity: u64) -> Result<u64> {
    capacity
        .checked_next_power_of_two()
        .map(|rounded| rounded.max(MIN_RING_CAPACITY))
        .ok_or(Error::CapacityExceeded)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capacity_rounds_to_power_of_two() {
        assert_eq!(round_capacity(64), Ok(64));
        assert_eq!(round_capacity(100), Ok(128));
        assert_eq!(round_capacity(1), Ok(64));
        assert_eq!(round_capacity(u64::MAX), Err(Error::CapacityExceeded));
    }

    #[test]
    #[expect(
        clippy::assertions_on_result_states,
        reason = "unwrap_used lint conflicts with clippy's suggested fix"
    )]
    fn mask_for_power_of_two_is_correct() {
        assert!(mask_for_capacity(512).is_ok());
        assert!(mask_for_capacity(3).is_err());
    }

    #[test]
    fn single_phase_write_read_round_trip() {
        let ring = RingBuf::create(64, ResourceKind::SharedMemory).expect("create");
        let pos = ring.reserve(12 + 5).expect("reserve"); // header + payload
        ring.write_frame(pos, b"hello", 42, 0).expect("write");

        let (header, payload) = ring.read_frame(pos).expect("read");
        assert_eq!(header.len, 5);
        assert_eq!(header.tag, 42);
        assert!(header.is_ready());
        assert_eq!(payload, b"hello");
    }

    #[test]
    fn generation_counter_advances_on_write() {
        let ring = RingBuf::create(64, ResourceKind::SharedMemory).expect("create");
        let gen_before = ring.generation().expect("gen");
        let pos = ring.reserve(12 + 3).expect("reserve");
        ring.write_frame(pos, b"abc", 0, 0).expect("write");
        let gen_after = ring.generation().expect("gen");
        assert!(gen_after > gen_before);
    }
}
