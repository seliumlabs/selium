//! Ring protocol layout: the single source of truth for the shared-memory
//! ring buffer protocol.
//!
//! This module defines the offset constants, frame codec, reservation
//! algorithm, reader/writer slot protocol, and [`RingReader`]/[`RingWriter`]
//! primitives generic over [`MappingBackend`]. Both guest-side code
//! (hardware atomics via [`PointerBackend`]) and host-side code (mutex-mediated
//! via `KernelBackend`) consume these primitives.
//!
//! # Atomicity contract
//!
//! Each ring is **single-writer-domain**: all writers on a given ring must
//! operate in the same atomicity domain (guest-side hardware atomics OR
//! host-side mutex-mediated, never mixed). Mixed-domain writes are
//! out-of-contract and may corrupt data. See `AGENTS.md`.

use std::sync::{
    Arc,
    atomic::{Ordering, fence},
};

use selium_memory::{MappingBackend, MemoryError, RING_HEADER_SIZE};
use selium_wire::error::{Error, Result};

// Re-export FrameHeader so the layout module is the canonical import path.
pub use selium_memory::FrameHeader;

/// Byte offset of the shared backpressure strategy (0 = Park, 1 = Drop).
pub const BACKPRESSURE_OFFSET: u64 = 1064;
/// Byte offset where ring buffer data begins (after the coordination header).
pub const DATA_OFFSET: u64 = RING_HEADER_SIZE;
/// Byte offset of the generation counter within the shared region.
pub const GENERATION_COUNTER_OFFSET: u64 = 0;
/// Maximum number of blocking reader slots available in the shared region.
pub const MAX_READER_SLOTS: usize = 128;
/// Maximum number of blocking writer slots available in the shared region.
pub const MAX_WRITER_SLOTS: usize = 128;
/// Minimum region size that can hold a ring buffer (coordination header + one
/// header-sized data area).
pub const MIN_REGION_BYTES: u64 = RING_HEADER_SIZE * 2;
/// Byte offset of the shared `next_tail` cursor (writers CAS to reserve space).
pub const NEXT_TAIL_OFFSET: u64 = 8;
/// Byte offset of the shared `next_writer_id` counter (fetch_add for unique writer IDs).
pub const NEXT_WRITER_ID_OFFSET: u64 = 1048;
/// Byte offset where the shared `reader_slots` array begins (128 × u64).
pub const READER_SLOTS_OFFSET: u64 = 24;
/// Byte offset of the shared `reader_slot_counter` (fetch_add for unique reader slot indices).
pub const READER_SLOT_COUNTER_OFFSET: u64 = 1056;
/// Byte offset of the shared ring buffer capacity in bytes.
pub const SHARED_CAPACITY_OFFSET: u64 = 1072;
/// Byte offset of the shared `writer_count` (incremented/decremented atomically).
pub const WRITER_COUNT_OFFSET: u64 = 16;
/// Byte offset where the shared `writer_slots` array begins (128 × u64).
pub const WRITER_SLOTS_OFFSET: u64 = 1080;
/// Byte offset of the shared `writer_slot_counter` (fetch_add for unique writer slot indices).
pub const WRITER_SLOT_COUNTER_OFFSET: u64 = 2104;

/// A monotonic cursor over a shared-memory ring buffer.
#[derive(Clone, Copy, Debug)]
pub struct Cursor {
    position: u64,
}

/// A reader over a shared-memory ring buffer, generic over [`MappingBackend`].
///
/// Tracks read position and optionally allocates a reader slot for
/// backpressure. The `backend` must be scoped to the ring's sub-region
/// (via `MappingBackend::sub_region` or equivalent).
pub struct RingReader {
    backend: Arc<dyn MappingBackend>,
    capacity: u64,
    mask: u64,
    pos: u64,
    reader_slot: Option<u32>,
}

/// A writer over a shared-memory ring buffer, generic over [`MappingBackend`].
///
/// The `backend` must be scoped to the ring's sub-region (via
/// `MappingBackend::sub_region` or equivalent).
///
/// # Atomicity contract
///
/// Each ring is **single-writer-domain**: all writers on a given ring MUST
/// operate within the same atomicity domain (guest hardware atomics OR host
/// mutex-mediated atomics, never mixed). See the crate-level docs and
/// `AGENTS.md` for details.
pub struct RingWriter {
    backend: Arc<dyn MappingBackend>,
    capacity: u64,
    mask: u64,
}

impl Cursor {
    /// Creates a new cursor at the given position.
    pub const fn new(position: u64) -> Self {
        Self { position }
    }

    /// Returns the current cursor position.
    pub fn get(&self) -> u64 {
        self.position
    }

    /// Advances the cursor by `delta` bytes.
    pub fn advance(&mut self, delta: u64) {
        self.position = self.position.wrapping_add(delta);
    }

    /// Returns the number of readable bytes between this cursor and `tail`.
    pub fn readable(&self, tail: u64) -> u64 {
        tail.saturating_sub(self.position)
    }

    /// Returns the number of writable bytes between `head` and this cursor using capacity.
    pub fn writable(&self, head: u64, capacity: u64) -> u64 {
        capacity.saturating_sub(self.position.wrapping_sub(head))
    }

    /// Computes the masked offset into the ring buffer for this position.
    pub fn masked(&self, mask: u64) -> u64 {
        self.position & mask
    }

    /// Computes the length of the tail segment (from masked position to end of buffer).
    pub fn tail_segment_len(&self, mask: u64) -> u64 {
        let masked = self.masked(mask);
        debug_assert!(masked <= mask.wrapping_add(1));
        mask.wrapping_add(1).wrapping_sub(masked)
    }

    /// Splits a write at `pos` of `len` bytes into two segments accounting for wraparound.
    /// Returns (tail_len, head_len) where tail is the amount before wraparound.
    pub fn split_wraparound(&self, len: u64, mask: u64) -> (u64, u64) {
        let tail_seg = self.tail_segment_len(mask).min(len);
        let head_seg = len.wrapping_sub(tail_seg);
        (tail_seg, head_seg)
    }
}

impl RingReader {
    /// Opens a reader on the given backend (already scoped to the ring's
    /// sub-region).
    ///
    /// When `allocate_slot` is true, allocates a reader slot through the shared
    /// `reader_slot_counter` for backpressure protection.
    pub fn open(
        backend: Arc<dyn MappingBackend>,
        capacity: u64,
        allocate_slot: bool,
    ) -> Result<Self> {
        let mask = mask_for_capacity(capacity)?;
        let reader_slot = if allocate_slot {
            Some(allocate_reader_slot(backend.as_ref(), 0)?)
        } else {
            None
        };
        Ok(Self {
            backend,
            capacity,
            mask,
            pos: 0,
            reader_slot,
        })
    }

    /// Returns the current read position.
    pub fn position(&self) -> u64 {
        self.pos
    }

    /// Returns the ring capacity.
    pub fn capacity(&self) -> u64 {
        self.capacity
    }

    /// Returns the allocated reader slot, if any.
    pub fn reader_slot(&self) -> Option<u32> {
        self.reader_slot
    }

    /// Returns a reference to the underlying backend.
    pub fn backend(&self) -> Arc<dyn MappingBackend> {
        self.backend.clone()
    }

    /// Loads the current generation counter.
    pub fn generation(&self) -> Result<u64> {
        load_generation(self.backend.as_ref())
    }

    /// Loads the shared `writer_count`.
    pub fn writer_count(&self) -> Result<u64> {
        load_writer_count(self.backend.as_ref())
    }

    /// Loads the shared `next_tail` cursor.
    pub fn next_tail(&self) -> Result<u64> {
        load_next_tail(self.backend.as_ref())
    }

    /// Reads the next frame if one is available. Returns `Ok(None)` if no
    /// ready frame exists.
    ///
    /// Advances the internal position by the frame size. If a reader slot is
    /// allocated, updates it to the new position.
    pub fn read_frame(&mut self) -> Result<Option<(FrameHeader, Vec<u8>)>> {
        let tail = self.next_tail()?;
        if self.pos >= tail {
            return Ok(None);
        }
        match read_frame(self.backend.as_ref(), self.pos, self.mask, self.capacity)? {
            Some((header, payload)) => {
                let frame_size = header.frame_size();
                self.pos = self.pos.checked_add(frame_size).ok_or_else(|| {
                    Error::InvalidFrame(format!(
                        "reader position overflow: {self_pos} + {frame_size}",
                        self_pos = self.pos
                    ))
                })?;
                if let Some(slot) = self.reader_slot {
                    update_reader_slot(self.backend.as_ref(), slot, self.pos)?;
                }
                Ok(Some((header, payload)))
            }
            None => Ok(None),
        }
    }

    /// Releases the reader slot (if allocated) and marks the reader as
    /// terminated.
    pub fn release(&mut self) -> Result<()> {
        if let Some(slot) = self.reader_slot {
            release_reader_slot(self.backend.as_ref(), slot)?;
            self.reader_slot = None;
        }
        Ok(())
    }
}

impl Drop for RingReader {
    fn drop(&mut self) {
        drop(self.release());
    }
}

impl RingWriter {
    /// Opens a writer on the given backend (already scoped to the ring's
    /// sub-region).
    pub fn open(backend: Arc<dyn MappingBackend>, capacity: u64) -> Result<Self> {
        let mask = mask_for_capacity(capacity)?;
        Ok(Self {
            backend,
            capacity,
            mask,
        })
    }

    /// Returns the ring capacity.
    pub fn capacity(&self) -> u64 {
        self.capacity
    }

    /// Returns a reference to the underlying backend.
    pub fn backend(&self) -> Arc<dyn MappingBackend> {
        self.backend.clone()
    }

    /// Loads the current generation counter.
    pub fn generation(&self) -> Result<u64> {
        load_generation(self.backend.as_ref())
    }

    /// Loads the shared `writer_count`.
    pub fn writer_count(&self) -> Result<u64> {
        load_writer_count(self.backend.as_ref())
    }

    /// Increments the shared `writer_count`.
    pub fn increment_writer_count(&self) -> Result<u64> {
        increment_writer_count(self.backend.as_ref())
    }

    /// Decrements the shared `writer_count`.
    pub fn decrement_writer_count(&self) -> Result<()> {
        decrement_writer_count(self.backend.as_ref())
    }

    /// Reserves `len` bytes at the tail with reader and writer backpressure.
    pub fn reserve(&self, len: u64, protect_readers: bool, protect_writers: bool) -> Result<u64> {
        reserve_tail(
            self.backend.as_ref(),
            len,
            self.capacity,
            protect_readers,
            protect_writers,
        )
    }

    /// Writes a framed message with the single-phase write protocol and bumps
    /// the generation counter.
    pub fn write_frame(&self, payload: &[u8], tag: u32, flags: u8) -> Result<()> {
        write_frame(
            self.backend.as_ref(),
            payload,
            tag,
            flags,
            self.capacity,
            self.mask,
            true,
            false,
        )
    }

    /// Writes a framed message with explicit backpressure control.
    pub fn write_frame_with_backpressure(
        &self,
        payload: &[u8],
        tag: u32,
        flags: u8,
        protect_readers: bool,
        protect_writers: bool,
    ) -> Result<()> {
        write_frame(
            self.backend.as_ref(),
            payload,
            tag,
            flags,
            self.capacity,
            self.mask,
            protect_readers,
            protect_writers,
        )
    }
}

/// Allocates a reader slot via the shared `reader_slot_counter` and initialises
/// it to `position`.
pub fn allocate_reader_slot(backend: &dyn MappingBackend, position: u64) -> Result<u32> {
    let slot_index = backend
        .fetch_add_u64(READER_SLOT_COUNTER_OFFSET, 1, Ordering::SeqCst)
        .map_err(Error::from)?;
    if slot_index >= MAX_READER_SLOTS as u64 {
        return Err(Error::CapacityExceeded);
    }
    let encoded = encode_reader_position(position)?;
    store_reader_slot(backend, slot_index as u32, encoded)?;
    Ok(slot_index as u32)
}

/// Allocates a writer id from the shared counter.
pub fn allocate_writer_id(backend: &dyn MappingBackend) -> Result<u32> {
    let id = backend
        .fetch_add_u64(NEXT_WRITER_ID_OFFSET, 1, Ordering::SeqCst)
        .map_err(Error::from)?;
    if id > u64::from(u32::MAX) {
        return Err(Error::CapacityExceeded);
    }
    Ok(id as u32)
}

/// Bumps the generation counter and returns the new value.
pub fn bump_generation(backend: &dyn MappingBackend) -> Result<u64> {
    let prev = backend
        .fetch_add_u64(GENERATION_COUNTER_OFFSET, 1, Ordering::Release)
        .map_err(Error::from)?;
    Ok(prev + 1)
}

/// CAS on the shared `next_tail` cursor. Returns the previous value.
pub fn cas_next_tail(backend: &dyn MappingBackend, current: u64, new: u64) -> Result<u64> {
    backend
        .compare_exchange_u64(NEXT_TAIL_OFFSET, current, new)
        .map_err(Error::from)
}

/// Decrements the shared `writer_count`.
pub fn decrement_writer_count(backend: &dyn MappingBackend) -> Result<()> {
    backend
        .fetch_add_u64(WRITER_COUNT_OFFSET, u64::MAX, Ordering::SeqCst)
        .map_err(Error::from)?;
    Ok(())
}

/// Weak-drains committed frames from a ring without holding a reader slot.
///
/// A "weak" drain never backpressures writers: it allocates no blocking
/// reader slot, so writers can always advance and a full ring overwrites the
/// oldest unread frames. When writers have overtaken `read_pos`, the drain
/// snaps forward to the newest window (skipping what was already lost) and
/// then reads every committed frame up to the write tail.
///
/// Returns the frame payloads (without headers) and the next read position.
/// Dropping at an uncommitted frame (header not yet READY) returns the
/// frames read so far: the caller retains the returned position and resumes
/// from it on the next call.
///
/// This is the read handle for log drains and other fire-and-forget
/// consumers where losing old records is acceptable but blocking a producer
/// is not.
///
/// **Torn reads are detected, not absorbed.** Every frame header carries an
/// FNV-1a checksum (see [`FrameHeader`]); when the snap-to-newest heuristic
/// lands mid-frame (possible after ring overwrite with large strides), the
/// bytes there fail verification and `read_frame` reports
/// [`Error::TornFrame`]. The drain then scans forward to the next
/// checksum-valid header boundary and resumes there, so delivery after the
/// torn point is exact. If no valid boundary exists ahead (a reserved but
/// not-yet-committed frame), the drain stops gracefully and returns the
/// position to resume from — the caller retries and observes the committed
/// frames on a later call. Callers that decode payloads best-effort should
/// still tolerate payload-level errors; header-level corruption never
/// surfaces as data.
pub fn drain_frames(
    backend: &dyn MappingBackend,
    mut read_pos: u64,
    capacity: u64,
) -> Result<(Vec<Vec<u8>>, u64)> {
    let mask = mask_for_capacity(capacity)?;
    let next_tail = load_next_tail(backend)?;

    // Never backpressure: if overtaken, jump to the newest window and skip
    // whatever the writers already overwrote.
    if next_tail > read_pos + capacity {
        read_pos = next_tail - capacity;
    }

    let mut frames = Vec::new();
    while read_pos < next_tail {
        match read_frame(backend, read_pos, mask, capacity) {
            Ok(Some((header, payload))) => {
                frames.push(payload);
                read_pos = read_pos
                    .checked_add(header.frame_size())
                    .ok_or(Error::InvalidFrame("frame size overflow".to_string()))?;
            }
            Ok(None) => break, // not-ready frame; resume here next drain
            Err(Error::TornFrame { .. }) => {
                // Snap landed mid-frame. Recover to the next checksum-valid
                // header boundary; without one ahead, this is the uncommitted
                // tail — stop and resume here on the next drain.
                match resync_frame_boundary(backend, read_pos, next_tail, mask, capacity)? {
                    Some(boundary) if boundary > read_pos => read_pos = boundary,
                    _ => break,
                }
            }
            Err(e) => return Err(e),
        }
    }

    Ok((frames, read_pos))
}

/// Encodes a reader position for storage in a shared slot (0 = unallocated).
pub fn encode_reader_position(position: u64) -> Result<u64> {
    position.checked_add(1).ok_or(Error::CapacityExceeded)
}

/// Encodes a writer position for storage in a shared slot (0 = unallocated).
pub fn encode_writer_position(position: u64) -> Result<u64> {
    position.checked_add(1).ok_or(Error::CapacityExceeded)
}

/// Increments the shared `writer_count`, returning the previous value.
pub fn increment_writer_count(backend: &dyn MappingBackend) -> Result<u64> {
    backend
        .fetch_add_u64(WRITER_COUNT_OFFSET, 1, Ordering::SeqCst)
        .map_err(Error::from)
}

/// Initialises all shared coordination fields to zero.
pub fn init_ring(backend: &dyn MappingBackend) -> Result<()> {
    backend.atomic_store_u64(GENERATION_COUNTER_OFFSET, 0, Ordering::Release)?;
    backend.atomic_store_u64(NEXT_TAIL_OFFSET, 0, Ordering::Release)?;
    backend.atomic_store_u64(WRITER_COUNT_OFFSET, 0, Ordering::Release)?;
    for i in 0..MAX_READER_SLOTS as u32 {
        let offset = READER_SLOTS_OFFSET + i as u64 * 8;
        backend.atomic_store_u64(offset, 0, Ordering::Release)?;
    }
    backend.atomic_store_u64(NEXT_WRITER_ID_OFFSET, 0, Ordering::Release)?;
    backend.atomic_store_u64(READER_SLOT_COUNTER_OFFSET, 0, Ordering::Release)?;
    for i in 0..MAX_WRITER_SLOTS as u32 {
        let offset = WRITER_SLOTS_OFFSET + i as u64 * 8;
        backend.atomic_store_u64(offset, 0, Ordering::Release)?;
    }
    backend.atomic_store_u64(WRITER_SLOT_COUNTER_OFFSET, 0, Ordering::Release)?;
    Ok(())
}

/// Loads the backpressure strategy.
pub fn load_backpressure(backend: &dyn MappingBackend) -> Result<u8> {
    let bytes = backend.read(BACKPRESSURE_OFFSET, 1).map_err(Error::from)?;
    bytes.first().copied().ok_or(Error::InvalidLayout)
}

/// Load the shared capacity from the ring's backend.
pub fn load_capacity(backend: &dyn MappingBackend) -> Result<u64> {
    load_shared_capacity(backend)
}

/// Loads the generation counter.
pub fn load_generation(backend: &dyn MappingBackend) -> Result<u64> {
    backend
        .atomic_load_u64(GENERATION_COUNTER_OFFSET, Ordering::Acquire)
        .map_err(Error::from)
}

/// Loads the shared `next_tail` cursor.
pub fn load_next_tail(backend: &dyn MappingBackend) -> Result<u64> {
    backend
        .atomic_load_u64(NEXT_TAIL_OFFSET, Ordering::Acquire)
        .map_err(Error::from)
}

/// Loads reader slot `slot`.
pub fn load_reader_slot(backend: &dyn MappingBackend, slot: u32) -> Result<u64> {
    if slot as usize >= MAX_READER_SLOTS {
        return Err(Error::InvalidLayout);
    }
    let offset = READER_SLOTS_OFFSET + slot as u64 * 8;
    backend
        .atomic_load_u64(offset, Ordering::Acquire)
        .map_err(Error::from)
}

/// Loads the shared capacity.
pub fn load_shared_capacity(backend: &dyn MappingBackend) -> Result<u64> {
    backend
        .atomic_load_u64(SHARED_CAPACITY_OFFSET, Ordering::Acquire)
        .map_err(Error::from)
}

/// Loads the shared `writer_count`.
pub fn load_writer_count(backend: &dyn MappingBackend) -> Result<u64> {
    backend
        .atomic_load_u64(WRITER_COUNT_OFFSET, Ordering::Acquire)
        .map_err(Error::from)
}

/// Loads writer slot `slot`.
pub fn load_writer_slot(backend: &dyn MappingBackend, slot: u32) -> Result<u64> {
    if slot as usize >= MAX_WRITER_SLOTS {
        return Err(Error::InvalidLayout);
    }
    let offset = WRITER_SLOTS_OFFSET + slot as u64 * 8;
    backend
        .atomic_load_u64(offset, Ordering::Acquire)
        .map_err(Error::from)
}

/// Computes a mask for a given capacity (must be a power of two).
pub fn mask_for_capacity(capacity: u64) -> Result<u64> {
    if !capacity.is_power_of_two() {
        return Err(Error::InvalidLayout);
    }
    Ok(capacity - 1)
}

/// Returns the minimum active reader position from the shared `reader_slots`
/// array.
pub fn minimum_reader_position(backend: &dyn MappingBackend) -> Result<Option<u64>> {
    let mut minimum = None;
    for slot in 0..MAX_READER_SLOTS as u32 {
        let offset = READER_SLOTS_OFFSET + slot as u64 * 8;
        let encoded = backend.atomic_load_u64(offset, Ordering::Acquire)?;
        if encoded == 0 {
            continue;
        }
        let position = encoded - 1;
        minimum = Some(minimum.map_or(position, |current: u64| current.min(position)));
    }
    Ok(minimum)
}

/// Returns the minimum active writer position from the shared `writer_slots`
/// array.
pub fn minimum_writer_position(backend: &dyn MappingBackend) -> Result<Option<u64>> {
    let mut minimum = None;
    for slot in 0..MAX_WRITER_SLOTS as u32 {
        let offset = WRITER_SLOTS_OFFSET + slot as u64 * 8;
        let encoded = backend.atomic_load_u64(offset, Ordering::Acquire)?;
        if encoded == 0 {
            continue;
        }
        let position = encoded - 1;
        minimum = Some(minimum.map_or(position, |current: u64| current.min(position)));
    }
    Ok(minimum)
}

/// Reads `len` bytes from the ring data area at logical position `pos`,
/// handling wraparound.
pub fn read_at(
    backend: &dyn MappingBackend,
    pos: u64,
    len: u64,
    mask: u64,
    capacity: u64,
) -> Result<Vec<u8>> {
    if len == 0 {
        return Ok(Vec::new());
    }
    if len > capacity {
        return Err(Error::InvalidFrame(format!(
            "read length {len} exceeds capacity {capacity}"
        )));
    }
    let raw_pos = DATA_OFFSET + (pos & mask);
    let ring_end = DATA_OFFSET + capacity;
    if raw_pos + len <= ring_end {
        backend.read(raw_pos, len).map_err(Error::from)
    } else {
        let tail_len = ring_end - raw_pos;
        let head_len = len - tail_len;
        let mut result = Vec::with_capacity(len as usize);
        let tail = backend.read(raw_pos, tail_len).map_err(Error::from)?;
        result.extend_from_slice(&tail);
        let head = backend.read(DATA_OFFSET, head_len).map_err(Error::from)?;
        result.extend_from_slice(&head);
        Ok(result)
    }
}

/// Reads a full framed message from `pos` with acquire fencing.
///
/// Returns `Ok(Some((header, payload)))` if the frame is ready, `Ok(None)` if
/// the frame is not yet committed (header not READY), or an error on invalid
/// frames.
pub fn read_frame(
    backend: &dyn MappingBackend,
    pos: u64,
    mask: u64,
    capacity: u64,
) -> Result<Option<(FrameHeader, Vec<u8>)>> {
    fence(Ordering::Acquire);
    let header_bytes = read_at(
        backend,
        pos,
        FrameHeader::ENCODED_SIZE as u64,
        mask,
        capacity,
    )?;
    let header = FrameHeader::decode(&header_bytes).map_err(|e| match e {
        MemoryError::CorruptedHeader => Error::TornFrame { position: pos },
        other => Error::InvalidFrame(other.to_string()),
    })?;

    if !header.is_ready() {
        return Ok(None);
    }

    let frame_size = header.frame_size();
    if frame_size > capacity {
        return Err(Error::InvalidFrame(format!(
            "frame size {frame_size} exceeds capacity {capacity}"
        )));
    }

    let payload_pos = pos
        .checked_add(FrameHeader::ENCODED_SIZE as u64)
        .ok_or_else(|| Error::InvalidFrame("payload position overflow".to_string()))?;
    let payload = read_at(backend, payload_pos, header.len as u64, mask, capacity)?;

    Ok(Some((header, payload)))
}

/// Reads a frame header at `pos` with acquire fencing.
pub fn read_frame_header(
    backend: &dyn MappingBackend,
    pos: u64,
    mask: u64,
    capacity: u64,
) -> Result<FrameHeader> {
    fence(Ordering::Acquire);
    let bytes = read_at(
        backend,
        pos,
        FrameHeader::ENCODED_SIZE as u64,
        mask,
        capacity,
    )?;
    FrameHeader::decode(&bytes).map_err(|e| match e {
        MemoryError::CorruptedHeader => Error::TornFrame { position: pos },
        other => Error::InvalidFrame(other.to_string()),
    })
}

/// Releases a reader slot (sets it to 0).
pub fn release_reader_slot(backend: &dyn MappingBackend, slot: u32) -> Result<()> {
    store_reader_slot(backend, slot, 0)
}

/// Releases a writer slot (sets it to 0).
pub fn release_writer_slot(backend: &dyn MappingBackend, slot: u32) -> Result<()> {
    store_writer_slot(backend, slot, 0)
}

/// Atomically reserves `len` bytes at the tail via CAS on `next_tail`.
///
/// Uses exponential backoff on contention. Checks backpressure against reader
/// and writer slots when `protect_readers` / `protect_writers` are set.
pub fn reserve_tail(
    backend: &dyn MappingBackend,
    len: u64,
    capacity: u64,
    protect_readers: bool,
    protect_writers: bool,
) -> Result<u64> {
    if len == 0 || len > capacity {
        return Err(Error::CapacityExceeded);
    }

    let mut delay: usize = 1;
    loop {
        let tail = load_next_tail(backend)?;
        let min_reader = if protect_readers {
            minimum_reader_position(backend)?
        } else {
            None
        };
        let min_writer = if protect_writers {
            minimum_writer_position(backend)?
        } else {
            None
        };
        let next = reserve_tail_next(
            tail,
            len,
            capacity,
            min_reader,
            min_writer,
            protect_readers,
            protect_writers,
        )?;

        let prev = cas_next_tail(backend, tail, next)?;
        if prev == tail {
            return Ok(tail);
        }

        // Exponential backoff on contention.
        for _ in 0..delay {
            std::hint::spin_loop();
        }
        delay = (delay * 2).min(64);
    }
}

/// Computes the next `next_tail` value after reserving `len` bytes.
///
/// Returns an error if the reservation would overflow or if backpressure
/// (reader/writer slot protection) prevents the reservation.
pub fn reserve_tail_next(
    tail: u64,
    len: u64,
    capacity: u64,
    minimum_reader_position: Option<u64>,
    minimum_writer_position: Option<u64>,
    protect_readers: bool,
    protect_writers: bool,
) -> Result<u64> {
    if len == 0 || len > capacity {
        return Err(Error::CapacityExceeded);
    }
    let next = tail
        .checked_add(len)
        .filter(|next| *next < u64::MAX)
        .ok_or(Error::CapacityExceeded)?;
    if protect_readers {
        let head = minimum_reader_position.unwrap_or(tail);
        if next.saturating_sub(head) > capacity {
            return Err(Error::BufferFull);
        }
    }
    if protect_writers {
        let head = minimum_writer_position.unwrap_or(tail);
        if next.saturating_sub(head) > capacity {
            return Err(Error::BufferFull);
        }
    }
    Ok(next)
}

/// Rounds a byte capacity to the next power of two for use as a ring buffer.
pub fn round_capacity(capacity: u64) -> Result<u64> {
    const MIN_RING_CAPACITY: u64 = 64;
    capacity
        .checked_next_power_of_two()
        .map(|rounded| rounded.max(MIN_RING_CAPACITY))
        .ok_or(Error::CapacityExceeded)
}

/// Stores the backpressure strategy (0 = Park, 1 = Drop).
pub fn store_backpressure(backend: &dyn MappingBackend, value: u8) -> Result<()> {
    backend
        .write(BACKPRESSURE_OFFSET, &[value])
        .map_err(Error::from)
}

/// Store the shared capacity on the ring's backend.
pub fn store_capacity(backend: &dyn MappingBackend, capacity: u64) -> Result<()> {
    store_shared_capacity(backend, capacity)
}

/// Stores a value into reader slot `slot`.
pub fn store_reader_slot(backend: &dyn MappingBackend, slot: u32, value: u64) -> Result<()> {
    if slot as usize >= MAX_READER_SLOTS {
        return Err(Error::InvalidLayout);
    }
    let offset = READER_SLOTS_OFFSET + slot as u64 * 8;
    backend
        .atomic_store_u64(offset, value, Ordering::Release)
        .map_err(Error::from)
}

/// Stores the shared capacity.
pub fn store_shared_capacity(backend: &dyn MappingBackend, capacity: u64) -> Result<()> {
    backend
        .atomic_store_u64(SHARED_CAPACITY_OFFSET, capacity, Ordering::Release)
        .map_err(Error::from)
}

/// Stores a value into writer slot `slot`.
pub fn store_writer_slot(backend: &dyn MappingBackend, slot: u32, value: u64) -> Result<()> {
    if slot as usize >= MAX_WRITER_SLOTS {
        return Err(Error::InvalidLayout);
    }
    let offset = WRITER_SLOTS_OFFSET + slot as u64 * 8;
    backend
        .atomic_store_u64(offset, value, Ordering::Release)
        .map_err(Error::from)
}

/// Updates an allocated reader slot to `position`.
pub fn update_reader_slot(backend: &dyn MappingBackend, slot: u32, position: u64) -> Result<()> {
    let encoded = encode_reader_position(position)?;
    store_reader_slot(backend, slot, encoded)
}

/// Updates an allocated writer slot to `position`.
pub fn update_writer_slot(backend: &dyn MappingBackend, slot: u32, position: u64) -> Result<()> {
    let encoded = encode_writer_position(position)?;
    store_writer_slot(backend, slot, encoded)
}

/// Writes `data` at logical position `pos` in the ring data area, handling
/// wraparound.
pub fn write_at(
    backend: &dyn MappingBackend,
    pos: u64,
    data: &[u8],
    mask: u64,
    capacity: u64,
) -> Result<()> {
    if data.len() as u64 > capacity {
        return Err(Error::BufferFull);
    }
    let raw_start = (pos & mask) + DATA_OFFSET;
    let ring_end = DATA_OFFSET + capacity;
    let tail = (data.len() as u64).min(ring_end - raw_start) as usize;
    let head = data.len() - tail;
    if tail > 0 {
        backend
            .write(raw_start, data.get(..tail).unwrap_or_default())
            .map_err(Error::from)?;
    }
    if head > 0 {
        backend
            .write(DATA_OFFSET, data.get(tail..).unwrap_or_default())
            .map_err(Error::from)?;
    }
    Ok(())
}

/// Writes a framed message using single-phase write with release fencing.
///
/// 1. Reserve `frame_size` bytes at the tail.
/// 2. Write a placeholder header (checksum-valid, not READY) at `pos`.
/// 3. Write payload at `pos + ENCODED_SIZE`.
/// 4. Release fence.
/// 5. Write header with READY flag at `pos`.
/// 6. Bump generation counter.
#[expect(
    clippy::too_many_arguments,
    reason = "ring-protocol write primitive; arguments are inherent to the protocol"
)]
///
/// 1. Reserve `frame_size` bytes at the tail.
/// 2. Write payload at `pos + ENCODED_SIZE`.
/// 3. Release fence.
/// 4. Write header with READY flag at `pos`.
/// 5. Bump generation counter.
pub fn write_frame(
    backend: &dyn MappingBackend,
    payload: &[u8],
    tag: u32,
    flags: u8,
    capacity: u64,
    mask: u64,
    protect_readers: bool,
    protect_writers: bool,
) -> Result<()> {
    let frame_size = FrameHeader::ENCODED_SIZE as u64 + payload.len() as u64;
    if frame_size > capacity {
        return Err(Error::CapacityExceeded);
    }

    let pos = reserve_tail(
        backend,
        frame_size,
        capacity,
        protect_readers,
        protect_writers,
    )?;

    // Placeholder header (checksum-valid, not READY) marks the reservation as
    // an in-flight frame. Readers gate on `is_ready()`; without this, a strict
    // checksum-verifying reader would read stale bytes at the frame start and
    // mistake an in-progress write for a torn header.
    let placeholder = FrameHeader {
        len: payload.len() as u32,
        tag,
        flags: flags & !FrameHeader::FLAG_READY,
    };
    write_at(backend, pos, &placeholder.encode(), mask, capacity)?;

    let payload_pos = pos
        .checked_add(FrameHeader::ENCODED_SIZE as u64)
        .ok_or_else(|| Error::InvalidFrame("payload position overflow".to_string()))?;
    write_at(backend, payload_pos, payload, mask, capacity)?;

    // Release fence ensures payload is visible before the header.
    fence(Ordering::Release);

    let ready_header = FrameHeader {
        len: payload.len() as u32,
        tag,
        flags: flags | FrameHeader::FLAG_READY,
    };
    write_at(backend, pos, &ready_header.encode(), mask, capacity)?;

    bump_generation(backend)?;

    Ok(())
}

/// Scans forward from `start` (exclusive) for the next position holding a
/// checksum-valid frame header, bounded by `end` (the write tail).
///
/// Used by [`drain_frames`] to recover from a torn read after the weak-drain
/// snapped into a partially overwritten region. Every byte up to the next
/// real frame boundary carries a checksum, so the scan stops exactly there;
/// the false-positive rate is a single `2⁻³²` checksum collision per probed
/// position.
fn resync_frame_boundary(
    backend: &dyn MappingBackend,
    start: u64,
    end: u64,
    mask: u64,
    capacity: u64,
) -> Result<Option<u64>> {
    let mut pos = start.wrapping_add(1);
    while pos < end {
        let bytes = read_at(
            backend,
            pos,
            FrameHeader::ENCODED_SIZE as u64,
            mask,
            capacity,
        )?;
        if let Ok(header) = FrameHeader::decode(&bytes)
            && header.frame_size() <= capacity
        {
            return Ok(Some(pos));
        }
        pos = pos.wrapping_add(1);
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use selium_memory::PointerBackend;

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
    }

    #[test]
    fn cursor_wraparound_splits() {
        let mask = 15;
        let c = Cursor::new(12);
        let (tail, head) = c.split_wraparound(10, mask);
        assert_eq!(tail, 4);
        assert_eq!(head, 6);
    }

    #[test]
    #[expect(
        clippy::assertions_on_result_states,
        reason = "unwrap_used lint conflicts with clippy's suggested fix"
    )]
    fn mask_rounds_down_capacity() {
        assert_eq!(mask_for_capacity(64).unwrap(), 63);
        assert_eq!(mask_for_capacity(1).unwrap(), 0);
        assert!(mask_for_capacity(3).is_err());
    }

    #[test]
    fn ring_reader_writer_round_trip_on_pointer_backend() {
        let backend = Arc::new(PointerBackend::allocate(DATA_OFFSET + 64).expect("allocate"));
        init_ring(backend.as_ref()).expect("init");
        store_shared_capacity(backend.as_ref(), 64).expect("store cap");

        let writer = RingWriter::open(backend.clone(), 64).expect("open writer");
        writer.increment_writer_count().expect("inc wc");
        writer.write_frame(b"hello", 42, 0).expect("write frame");

        let mut reader = RingReader::open(backend.clone(), 64, true).expect("open reader");
        let (header, payload) = reader.read_frame().expect("read").expect("got frame");
        assert_eq!(payload, b"hello");
        assert_eq!(header.tag, 42);
        assert!(header.is_ready());

        assert!(reader.read_frame().expect("read2").is_none());
    }

    #[test]
    fn ring_reader_allocates_slot_via_counter() {
        let backend = Arc::new(PointerBackend::allocate(DATA_OFFSET + 64).expect("allocate"));
        init_ring(backend.as_ref()).expect("init");

        let reader = RingReader::open(backend.clone(), 64, true).expect("open reader");
        let slot = reader.reader_slot().expect("slot allocated");
        assert_eq!(slot, 0); // first allocation from counter

        let encoded = load_reader_slot(backend.as_ref(), 0).expect("load slot");
        assert_eq!(encoded, 1); // encoded position 0 = 1
    }
}
