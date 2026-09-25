use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use selium_abi::{RegionProt, ResourceKind};
use selium_memory::{Region, RegionMapping, WASM_PAGE_SIZE};
use selium_wire::error::{Error, Result};

use crate::layout::{
    self, allocate_reader_slot, allocate_writer_id, cas_next_tail, init_ring, load_generation,
    load_next_tail, load_writer_count, minimum_reader_position, minimum_writer_position,
    release_reader_slot, reserve_tail, store_backpressure, store_reader_slot,
    store_shared_capacity, store_writer_slot, update_reader_slot, update_writer_slot,
};

// Re-export all layout constants for backward compatibility.
pub use crate::layout::{
    BACKPRESSURE_OFFSET, DATA_OFFSET, GENERATION_COUNTER_OFFSET, MAX_READER_SLOTS,
    MAX_WRITER_SLOTS, MIN_REGION_BYTES, NEXT_TAIL_OFFSET, NEXT_WRITER_ID_OFFSET,
    READER_SLOT_COUNTER_OFFSET, READER_SLOTS_OFFSET, SHARED_CAPACITY_OFFSET, WRITER_COUNT_OFFSET,
    WRITER_SLOT_COUNTER_OFFSET, WRITER_SLOTS_OFFSET, encode_reader_position,
    encode_writer_position, reserve_tail_next,
};

/// A shared memory region allocated for ring buffer I/O.
///
/// The shared region contains cross-process coordination fields in page 0:
/// generation counter (offset 0), `next_tail` (offset 8), `writer_count`
/// (offset 16), `reader_slots` (128 × u64 at offset 24), `next_writer_id`
/// (offset 1048), `reader_slot_counter` (offset 1056), `backpressure`
/// (offset 1064), `capacity` (offset 1072), `writer_slots` (128 × u64 at
/// offset 1080), and `writer_slot_counter` (offset 2104). Ring buffer data
/// starts at page 1 (offset 4096).
///
/// Process-local optimisation fields (`tail_cache`, `next_mutation_id`) live
/// in per-guest private memory via `ChannelPrivateState`.
#[derive(Clone)]
pub struct ChannelRegion {
    region_id: u64,
    mapping: RegionMapping,
    /// The underlying shared region. Retained so that hostcall-mediated slot
    /// operations (e.g. `Region::write_table_slot`) can be delegated
    /// through `ChannelRegion` for consumer guests with read-only mappings.
    /// `None` for sub-mappings carved from a parent (e.g. RPC multi-memory).
    region: Option<Arc<Region>>,
    private: Arc<ChannelPrivateState>,
    capacity: u64,
    size: u64,
}

/// Per-guest private channel state. Not stored in shared memory.
struct ChannelPrivateState {
    tail_cache: AtomicU64,
    next_mutation_id: AtomicU64,
}

impl ChannelRegion {
    /// Creates a new channel region by allocating a shared region via
    /// [`Region::allocate`].
    pub fn create(capacity: u64, purpose: ResourceKind) -> Result<Self> {
        #[cfg(test)]
        crate::ensure_heap_provider();
        let total_aligned = aligned_region_size(capacity)?;
        let pages = pages_for_bytes(total_aligned);
        let region = crate::allocate_region(pages, RegionProt::ReadWrite, purpose)?;
        let mapping = region.mapping();
        Ok(Self {
            region_id: region.region_id(),
            mapping,
            region: Some(Arc::new(region)),
            private: Arc::new(ChannelPrivateState::default()),
            capacity,
            size: total_aligned,
        })
    }

    /// Attaches to an existing channel region by its shared region id.
    pub fn attach(region_id: u64) -> Result<Self> {
        #[cfg(test)]
        crate::ensure_heap_provider();
        let region = crate::attach_region(region_id, None, RegionProt::ReadWrite)?;
        let mapping = region.mapping();

        let capacity = layout::load_shared_capacity(mapping.backend())?;
        if capacity == 0 {
            return Err(Error::InvalidLayout);
        }

        let size = region.size();
        Ok(Self {
            region_id,
            mapping,
            region: Some(Arc::new(region)),
            private: Arc::new(ChannelPrivateState::default()),
            capacity,
            size,
        })
    }

    /// Wraps an existing region mapping as a channel region.
    pub fn from_mapping(mapping: RegionMapping, capacity: u64) -> Self {
        Self::from_mapping_with_id(mapping, capacity, 0)
    }

    /// Like [`Self::from_mapping`], but records the shared region id so
    /// generation-wait registrations name the correct region. Callers that
    /// attach a known shared region MUST supply its id; otherwise
    /// `WaitRegister`-style wakeups cannot be routed.
    pub fn from_mapping_with_id(mapping: RegionMapping, capacity: u64, region_id: u64) -> Self {
        let size = DATA_OFFSET + capacity;
        Self {
            region_id,
            mapping,
            region: None,
            private: Arc::new(ChannelPrivateState::default()),
            capacity,
            size,
        }
    }

    /// Returns a reference to the underlying shared region, if available.
    pub fn shared_region(&self) -> Option<&Region> {
        self.region.as_deref()
    }

    /// Returns the shared region id.
    pub fn region_id(&self) -> u64 {
        self.region_id
    }

    /// Returns the ring data capacity in bytes.
    pub fn capacity(&self) -> u64 {
        self.capacity
    }

    /// Returns the total region size in bytes.
    pub fn size(&self) -> u64 {
        self.size
    }

    /// Returns the data offset within the region where ring bytes start.
    pub fn data_offset(&self) -> u64 {
        DATA_OFFSET
    }

    /// Returns a reference to the underlying region mapping.
    pub fn mapping(&self) -> &RegionMapping {
        &self.mapping
    }

    /// Convenience: returns the inner backend for layout-level operations.
    pub(crate) fn backend(&self) -> &dyn selium_memory::MappingBackend {
        self.mapping.backend()
    }

    /// Loads the generation counter from shared memory.
    pub fn load_generation(&self) -> Result<u64> {
        load_generation(self.backend())
    }

    /// Increments the generation counter in shared memory and notifies waiters.
    pub fn bump_generation(&self) -> Result<u64> {
        let new_gen = layout::bump_generation(self.backend())?;
        // Wake count is 1: exactly one host drainer parks on the ring's
        // generation word (guest tasks park via `register_generation_wait`
        // below, which never uses `memory.atomic.wait`). The count was
        // u32::MAX when the notify was a stable no-op; a genuine
        // `memory.atomic.notify` would make the engine iterate the count
        // under the waiter registry's read lock, so keep it minimal and
        // exact.
        drop(self.mapping.atomic_notify(GENERATION_COUNTER_OFFSET, 1));
        selium_memory::wake_generation_waiters(self.region_id, new_gen);
        Ok(new_gen)
    }

    /// Loads the shared `next_tail` cursor from shared memory.
    pub fn load_next_tail(&self) -> Result<u64> {
        load_next_tail(self.backend())
    }

    /// Atomically CAS on the shared `next_tail` cursor.
    pub fn cas_next_tail(&self, current: u64, new: u64) -> Result<u64> {
        cas_next_tail(self.backend(), current, new)
    }

    /// Reads the next_tail cursor from shared memory (alias for `load_next_tail`).
    pub fn read_next_tail(&self) -> Result<u64> {
        self.load_next_tail()
    }

    /// Writes the next_tail cursor to shared memory.
    pub fn write_next_tail(&self, value: u64) -> Result<()> {
        self.mapping
            .atomic_store_u64(NEXT_TAIL_OFFSET, value, Ordering::Release)
            .map_err(Error::from)
    }

    /// Reads the tail_cache from private state.
    pub fn read_tail_cache(&self) -> Result<u64> {
        Ok(self.private.tail_cache.load(Ordering::Acquire))
    }

    /// Loads the shared `writer_count` from shared memory.
    pub fn load_writer_count(&self) -> Result<u64> {
        load_writer_count(self.backend())
    }

    /// Atomically adds to the shared `writer_count`, returning the previous value.
    pub fn fetch_add_writer_count(&self, delta: u64) -> Result<u64> {
        self.mapping
            .fetch_add_u64(WRITER_COUNT_OFFSET, delta, Ordering::SeqCst)
            .map_err(Error::from)
    }

    /// Reads the writer count from shared memory.
    pub fn read_writer_count(&self) -> Result<u64> {
        self.load_writer_count()
    }

    /// Increments the shared writer count and returns the previous value.
    pub fn increment_writer_count(&self) -> Result<u64> {
        self.fetch_add_writer_count(1)
    }

    /// Decrements the shared writer count.
    pub fn decrement_writer_count(&self) -> Result<()> {
        self.fetch_add_writer_count(u64::MAX)?;
        Ok(())
    }

    /// Loads a reader slot value from the shared `reader_slots` array.
    pub fn load_reader_slot(&self, slot: u32) -> Result<u64> {
        layout::load_reader_slot(self.backend(), slot)
    }

    /// Stores a value into a reader slot in the shared `reader_slots` array.
    pub fn store_reader_slot(&self, slot: u32, value: u64) -> Result<()> {
        store_reader_slot(self.backend(), slot, value)
    }

    /// Atomically increments the shared `next_writer_id` counter, returning the previous value.
    pub fn fetch_add_next_writer_id(&self) -> Result<u64> {
        self.mapping
            .fetch_add_u64(NEXT_WRITER_ID_OFFSET, 1, Ordering::SeqCst)
            .map_err(Error::from)
    }

    /// Atomically increments the shared `reader_slot_counter`, returning the previous value.
    pub fn fetch_add_reader_slot_counter(&self) -> Result<u64> {
        self.mapping
            .fetch_add_u64(READER_SLOT_COUNTER_OFFSET, 1, Ordering::SeqCst)
            .map_err(Error::from)
    }

    /// Allocates a stable writer id from the shared counter.
    pub fn allocate_writer_id(&self) -> Result<u32> {
        allocate_writer_id(self.backend())
    }

    /// Allocates a globally unique mutation id from private state.
    pub fn allocate_mutation_id(&self) -> Result<u64> {
        Ok(self.private.next_mutation_id.fetch_add(1, Ordering::SeqCst) + 1)
    }

    /// Reads the blocking reader count by scanning shared reader slots.
    pub fn read_reader_count(&self) -> Result<u64> {
        let mut count = 0;
        for i in 0..MAX_READER_SLOTS as u32 {
            if self.load_reader_slot(i)? != 0 {
                count += 1;
            }
        }
        Ok(count)
    }

    /// Allocates a reader cursor slot via the shared `reader_slot_counter`.
    pub fn allocate_reader_slot(&self, position: u64) -> Result<u32> {
        allocate_reader_slot(self.backend(), position)
    }

    /// Updates an allocated reader cursor slot.
    pub fn update_reader_slot(&self, slot: u32, position: u64) -> Result<()> {
        update_reader_slot(self.backend(), slot, position)
    }

    /// Releases an allocated reader cursor slot.
    pub fn release_reader_slot(&self, slot: u32) -> Result<()> {
        release_reader_slot(self.backend(), slot)
    }

    /// Returns the minimum active blocking-reader cursor from shared reader slots.
    pub fn minimum_reader_position(&self) -> Result<Option<u64>> {
        minimum_reader_position(self.backend())
    }

    /// Loads a writer slot value from the shared `writer_slots` array.
    pub fn load_writer_slot(&self, slot: u32) -> Result<u64> {
        if slot as usize >= MAX_WRITER_SLOTS {
            return Err(Error::InvalidLayout);
        }
        let offset = WRITER_SLOTS_OFFSET + slot as u64 * 8;
        self.mapping
            .atomic_load_u64(offset, Ordering::Acquire)
            .map_err(Error::from)
    }

    /// Stores a value into a writer slot in the shared `writer_slots` array.
    pub fn store_writer_slot(&self, slot: u32, value: u64) -> Result<()> {
        store_writer_slot(self.backend(), slot, value)
    }

    /// Atomically increments the shared `writer_slot_counter`, returning the previous value.
    pub fn fetch_add_writer_slot_counter(&self) -> Result<u64> {
        self.mapping
            .fetch_add_u64(WRITER_SLOT_COUNTER_OFFSET, 1, Ordering::SeqCst)
            .map_err(Error::from)
    }

    /// Allocates a writer cursor slot via the shared `writer_slot_counter`.
    pub fn allocate_writer_slot(&self, position: u64) -> Result<u32> {
        let slot_index = self.fetch_add_writer_slot_counter()?;
        if slot_index >= MAX_WRITER_SLOTS as u64 {
            return Err(Error::CapacityExceeded);
        }
        let encoded = encode_writer_position(position)?;
        self.store_writer_slot(slot_index as u32, encoded)?;
        Ok(slot_index as u32)
    }

    /// Updates an allocated writer cursor slot.
    pub fn update_writer_slot(&self, slot: u32, position: u64) -> Result<()> {
        update_writer_slot(self.backend(), slot, position)
    }

    /// Releases an allocated writer cursor slot.
    pub fn release_writer_slot(&self, slot: u32) -> Result<()> {
        self.store_writer_slot(slot, 0)
    }

    /// Returns the minimum active blocking-writer cursor from shared writer slots.
    pub fn minimum_writer_position(&self) -> Result<Option<u64>> {
        minimum_writer_position(self.backend())
    }

    /// Atomically reserves `len` bytes at the tail, returning the reservation position.
    pub fn reserve_tail(
        &self,
        len: u64,
        protect_readers: bool,
        protect_writers: bool,
    ) -> Result<u64> {
        reserve_tail(
            self.backend(),
            len,
            self.capacity,
            protect_readers,
            protect_writers,
        )
    }

    /// Reads bytes from the ring data area.
    pub fn read_data(&self, offset: u64, len: u64) -> Result<Vec<u8>> {
        let data_offset = DATA_OFFSET + offset;
        self.mapping.read(data_offset, len).map_err(Error::from)
    }

    /// Writes bytes to the ring data area.
    pub fn write_data(&self, offset: u64, bytes: &[u8]) -> Result<()> {
        let data_offset = DATA_OFFSET + offset;
        self.mapping.write(data_offset, bytes).map_err(Error::from)
    }

    /// Loads the shared backpressure strategy from the channel header.
    pub fn load_backpressure(&self) -> Result<u8> {
        layout::load_backpressure(self.backend())
    }

    /// Stores the shared backpressure strategy into the channel header.
    pub fn store_backpressure(&self, value: u8) -> Result<()> {
        store_backpressure(self.backend(), value)
    }

    /// Loads the shared ring buffer capacity from the channel header.
    pub fn load_shared_capacity(&self) -> Result<u64> {
        layout::load_shared_capacity(self.backend())
    }

    /// Stores the ring buffer capacity into the channel header.
    pub fn store_shared_capacity(&self, capacity: u64) -> Result<()> {
        store_shared_capacity(self.backend(), capacity)
    }

    /// Initialises a fresh region with all shared coordination fields zeroed.
    pub fn initialise(&self) -> Result<()> {
        init_ring(self.backend())
    }
}

impl Default for ChannelPrivateState {
    fn default() -> Self {
        Self {
            tail_cache: AtomicU64::new(0),
            next_mutation_id: AtomicU64::new(0),
        }
    }
}

fn aligned_region_size(capacity: u64) -> Result<u64> {
    let total = DATA_OFFSET
        .checked_add(capacity)
        .ok_or(Error::CapacityExceeded)?;
    Ok(total
        .checked_next_power_of_two()
        .ok_or(Error::CapacityExceeded)?
        .max(MIN_REGION_BYTES))
}

/// Computes the number of WASM pages needed to hold `bytes`.
fn pages_for_bytes(bytes: u64) -> u32 {
    bytes.div_ceil(WASM_PAGE_SIZE) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reader_position_encoding_reserves_zero_for_empty_slots() {
        assert_eq!(encode_reader_position(0), Ok(1));
        assert_eq!(
            encode_reader_position(u64::MAX),
            Err(Error::CapacityExceeded)
        );
    }

    #[test]
    fn aligned_region_size_accounts_for_header_and_limits() {
        assert_eq!(aligned_region_size(1), Ok(MIN_REGION_BYTES));
        assert_eq!(aligned_region_size(4096), Ok(8192));
        assert_eq!(aligned_region_size(u64::MAX), Err(Error::CapacityExceeded));
        assert_eq!(
            aligned_region_size(u64::MAX - DATA_OFFSET),
            Err(Error::CapacityExceeded)
        );
    }

    #[test]
    fn reserve_tail_next_checks_capacity_and_overflow() {
        assert_eq!(
            reserve_tail_next(10, 8, 64, None, None, false, false),
            Ok(18)
        );
        assert_eq!(
            reserve_tail_next(10, 0, 64, None, None, false, false),
            Err(Error::CapacityExceeded)
        );
        assert_eq!(
            reserve_tail_next(u64::MAX - 4, 4, 64, None, None, false, false),
            Err(Error::CapacityExceeded)
        );
        assert_eq!(
            reserve_tail_next(100, 20, 64, Some(40), None, true, false),
            Err(Error::BufferFull)
        );
        assert_eq!(
            reserve_tail_next(100, 20, 64, Some(60), None, true, false),
            Ok(120)
        );
        assert_eq!(
            reserve_tail_next(100, 20, 64, None, Some(40), false, true),
            Err(Error::BufferFull)
        );
        assert_eq!(
            reserve_tail_next(100, 20, 64, None, Some(60), false, true),
            Ok(120)
        );
        assert_eq!(
            reserve_tail_next(100, 20, 64, Some(60), Some(40), true, true),
            Err(Error::BufferFull)
        );
        assert_eq!(
            reserve_tail_next(100, 20, 64, Some(60), Some(60), true, true),
            Ok(120)
        );
    }

    #[test]
    fn channel_region_reserve_tail_with_backoff() {
        let region = ChannelRegion::create(64, ResourceKind::SharedMemory).expect("create");
        region.initialise().expect("init");
        let pos = region.reserve_tail(8, false, false).expect("reserve");
        assert_eq!(pos, 0);
        let pos2 = region.reserve_tail(8, false, false).expect("reserve");
        assert_eq!(pos2, 8);
    }

    #[test]
    fn exponential_backoff_under_concurrent_contention() {
        let region = std::sync::Arc::new(
            ChannelRegion::create(4096, ResourceKind::SharedMemory).expect("create"),
        );
        region.initialise().expect("init");

        let mut handles = Vec::new();
        let thread_count = 8;
        let reservations_per_thread = 32;

        for _ in 0..thread_count {
            let r = region.clone();
            handles.push(std::thread::spawn(move || {
                for _ in 0..reservations_per_thread {
                    let pos = r
                        .reserve_tail(8, false, false)
                        .expect("reserve under contention");
                    assert_eq!(pos % 8, 0, "reservation {pos} not aligned");
                }
            }));
        }

        for h in handles {
            h.join().expect("thread panicked");
        }
    }

    #[test]
    fn two_writers_coordinate_on_shared_next_tail() {
        let region_a = ChannelRegion::create(4096, ResourceKind::SharedMemory).expect("create");
        region_a.initialise().expect("init");
        let region_b = region_a.clone();

        let pos_a = region_a.reserve_tail(16, false, false).expect("reserve a");
        let pos_b = region_b.reserve_tail(16, false, false).expect("reserve b");

        assert_ne!(pos_a, pos_b);
        assert!(
            pos_a + 16 <= pos_b || pos_b + 16 <= pos_a,
            "reservations must not overlap"
        );

        assert_eq!(
            region_a.load_next_tail().expect("tail a"),
            region_b.load_next_tail().expect("tail b"),
        );
    }

    #[test]
    fn reader_sees_writer_count_from_cloned_region() {
        let region_a = ChannelRegion::create(64, ResourceKind::SharedMemory).expect("create");
        region_a.initialise().expect("init");
        let region_b = region_a.clone();

        assert_eq!(region_a.load_writer_count().expect("wc a"), 0);
        region_a.increment_writer_count().expect("inc");
        assert_eq!(region_b.load_writer_count().expect("wc b"), 1);

        region_a.decrement_writer_count().expect("dec");
        assert_eq!(region_b.load_writer_count().expect("wc b after dec"), 0);
    }

    #[test]
    fn writer_backpressure_from_shared_reader_slots() {
        let region = ChannelRegion::create(64, ResourceKind::SharedMemory).expect("create");
        region.initialise().expect("init");

        let slot = region.allocate_reader_slot(0).expect("alloc slot");

        let mut total_reserved = 0u64;
        loop {
            match region.reserve_tail(8, true, false) {
                Ok(_pos) => total_reserved += 8,
                Err(Error::BufferFull) => break,
                Err(e) => panic!("unexpected error: {e}"),
            }
        }

        assert!(
            total_reserved <= 64,
            "reserved {total_reserved} bytes but capacity is 64"
        );

        region
            .update_reader_slot(slot, total_reserved)
            .expect("update slot");

        let pos = region
            .reserve_tail(8, true, false)
            .expect("reserve after advance");
        assert!(pos >= total_reserved);
    }

    #[test]
    fn reader_detects_eof_when_writer_count_reaches_zero() {
        let region = ChannelRegion::create(64, ResourceKind::SharedMemory).expect("create");
        region.initialise().expect("init");

        assert_eq!(region.load_writer_count().expect("wc"), 0);
        region.increment_writer_count().expect("inc");
        assert_eq!(region.load_writer_count().expect("wc"), 1);
        region.decrement_writer_count().expect("dec");
        assert_eq!(region.load_writer_count().expect("wc after dec"), 0);
    }

    #[test]
    fn shared_reader_slot_counter_allocates_unique_indices() {
        let region_a = ChannelRegion::create(4096, ResourceKind::SharedMemory).expect("create");
        region_a.initialise().expect("init");
        let region_b = region_a.clone();

        let slot_a = region_a.allocate_reader_slot(0).expect("alloc a");
        let slot_b = region_b.allocate_reader_slot(0).expect("alloc b");

        assert_ne!(slot_a, slot_b, "slot indices must be unique");
    }

    #[test]
    fn shared_writer_id_allocation() {
        let region_a = ChannelRegion::create(64, ResourceKind::SharedMemory).expect("create");
        region_a.initialise().expect("init");
        let region_b = region_a.clone();

        let id_a = region_a.allocate_writer_id().expect("id a");
        let id_b = region_b.allocate_writer_id().expect("id b");

        assert_ne!(id_a, id_b, "writer IDs must be unique");
    }

    #[test]
    fn shared_header_capacity_round_trip() {
        let region = ChannelRegion::create(4096, ResourceKind::SharedMemory).expect("create");
        region.initialise().expect("init");

        assert_eq!(region.load_shared_capacity().expect("load"), 0);
        region.store_shared_capacity(4096).expect("store");
        assert_eq!(region.load_shared_capacity().expect("load"), 4096);
    }

    #[test]
    fn shared_header_backpressure_round_trip() {
        let region = ChannelRegion::create(64, ResourceKind::SharedMemory).expect("create");
        region.initialise().expect("init");

        assert_eq!(region.load_backpressure().expect("load"), 0);
        region.store_backpressure(1).expect("store");
        assert_eq!(region.load_backpressure().expect("load"), 1);
        region.store_backpressure(0).expect("store");
        assert_eq!(region.load_backpressure().expect("load"), 0);
    }

    #[test]
    fn shared_header_visible_across_cloned_regions() {
        let region_a = ChannelRegion::create(4096, ResourceKind::SharedMemory).expect("create");
        region_a.initialise().expect("init");
        let region_b = region_a.clone();

        region_a.store_shared_capacity(4096).expect("store cap");
        region_a.store_backpressure(1).expect("store bp");

        assert_eq!(region_b.load_shared_capacity().expect("load cap"), 4096);
        assert_eq!(region_b.load_backpressure().expect("load bp"), 1);
    }

    #[test]
    fn create_assigns_unique_region_ids() {
        let a = ChannelRegion::create(64, ResourceKind::SharedMemory).expect("create a");
        let b = ChannelRegion::create(64, ResourceKind::SharedMemory).expect("create b");
        assert_ne!(a.region_id(), 0);
        assert_ne!(b.region_id(), 0);
        assert_ne!(a.region_id(), b.region_id());
    }

    #[test]
    fn attach_shares_memory_and_reads_capacity() {
        let original = ChannelRegion::create(4096, ResourceKind::SharedMemory).expect("create");
        original.initialise().expect("init");
        original.store_shared_capacity(4096).expect("store cap");

        let region_id = original.region_id();

        original.write_data(0, b"shared!").expect("write");

        let attached = ChannelRegion::attach(region_id).expect("attach");
        assert_eq!(attached.capacity(), 4096);

        let data = attached.read_data(0, 7).expect("read");
        assert_eq!(data, b"shared!");

        attached.write_data(8, b"hello").expect("write via attach");
        let data = original.read_data(8, 5).expect("read original");
        assert_eq!(data, b"hello");
    }

    #[test]
    fn attach_unknown_region_fails() {
        let result = ChannelRegion::attach(999_999_999);
        assert!(matches!(result, Err(Error::InvalidRegion)));
    }

    #[test]
    fn attach_zero_capacity_fails() {
        let original = ChannelRegion::create(64, ResourceKind::SharedMemory).expect("create");
        original.initialise().expect("init");

        let result = ChannelRegion::attach(original.region_id());
        assert!(matches!(result, Err(Error::InvalidLayout)));
    }

    #[test]
    fn writer_position_encoding_reserves_zero_for_empty_slots() {
        assert_eq!(encode_writer_position(0), Ok(1));
        assert_eq!(
            encode_writer_position(u64::MAX),
            Err(Error::CapacityExceeded)
        );
    }

    #[test]
    fn writer_slot_allocate_update_release() {
        let region = ChannelRegion::create(64, ResourceKind::SharedMemory).expect("create");
        region.initialise().expect("init");

        let slot = region.allocate_writer_slot(0).expect("alloc");
        assert_eq!(slot, 0);

        let encoded = region.load_writer_slot(slot).expect("load");
        assert_eq!(encoded, 1);

        region.update_writer_slot(slot, 42).expect("update");
        let encoded = region.load_writer_slot(slot).expect("load");
        assert_eq!(encoded, 43);

        region.release_writer_slot(slot).expect("release");
        let encoded = region.load_writer_slot(slot).expect("load");
        assert_eq!(encoded, 0);
    }

    #[test]
    fn minimum_writer_position_scans_slots() {
        let region = ChannelRegion::create(64, ResourceKind::SharedMemory).expect("create");
        region.initialise().expect("init");

        assert_eq!(region.minimum_writer_position().expect("min"), None);

        let _slot_a = region.allocate_writer_slot(10).expect("alloc a");
        let _slot_b = region.allocate_writer_slot(30).expect("alloc b");

        assert_eq!(region.minimum_writer_position().expect("min"), Some(10));

        region.update_writer_slot(0, 50).expect("update a");
        assert_eq!(region.minimum_writer_position().expect("min"), Some(30));

        region.release_writer_slot(1).expect("release b");
        assert_eq!(region.minimum_writer_position().expect("min"), Some(50));

        region.release_writer_slot(0).expect("release a");
        assert_eq!(region.minimum_writer_position().expect("min"), None);
    }

    #[test]
    fn writer_slot_counter_allocates_unique_indices() {
        let region_a = ChannelRegion::create(4096, ResourceKind::SharedMemory).expect("create");
        region_a.initialise().expect("init");
        let region_b = region_a.clone();

        let slot_a = region_a.allocate_writer_slot(0).expect("alloc a");
        let slot_b = region_b.allocate_writer_slot(0).expect("alloc b");

        assert_ne!(slot_a, slot_b, "slot indices must be unique");
    }

    #[test]
    fn writer_backpressure_from_shared_writer_slots() {
        let region = ChannelRegion::create(64, ResourceKind::SharedMemory).expect("create");
        region.initialise().expect("init");

        let slot = region.allocate_writer_slot(0).expect("alloc slot");

        let mut total_reserved = 0u64;
        loop {
            match region.reserve_tail(8, false, true) {
                Ok(_pos) => total_reserved += 8,
                Err(Error::BufferFull) => break,
                Err(e) => panic!("unexpected error: {e}"),
            }
        }

        assert!(
            total_reserved <= 64,
            "reserved {total_reserved} bytes but capacity is 64"
        );

        region
            .update_writer_slot(slot, total_reserved)
            .expect("update slot");

        let pos = region
            .reserve_tail(8, false, true)
            .expect("reserve after advance");
        assert!(pos >= total_reserved);
    }

    #[test]
    fn writer_slots_visible_across_cloned_regions() {
        let region_a = ChannelRegion::create(64, ResourceKind::SharedMemory).expect("create");
        region_a.initialise().expect("init");
        let region_b = region_a.clone();

        let slot = region_a.allocate_writer_slot(100).expect("alloc");
        assert_eq!(region_b.load_writer_slot(slot).expect("load b"), 101);

        region_a.update_writer_slot(slot, 200).expect("update");
        assert_eq!(region_b.load_writer_slot(slot).expect("load b"), 201);
    }
}
