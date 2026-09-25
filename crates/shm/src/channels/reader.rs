#![expect(
    clippy::indexing_slicing,
    reason = "pending_frame offsets are bounds-checked before access"
)]

use std::{
    pin::Pin,
    sync::atomic::{Ordering, fence},
    task::{Context, Poll},
};

use selium_memory::FrameHeader;
use selium_wire::error::{Error, Result};
use tokio::io::{AsyncRead, ReadBuf};

use crate::{channels::ChannelBackpressure, region::ChannelRegion};

/// Trait for reader types that can report their ring buffer generation counter.
///
/// Both [`Reader`] and [`BlockingReader`] implement this, allowing framed types
/// to work generically over non-blocking and blocking read handles respectively.
pub trait HasGeneration {
    /// Returns the current generation counter from the underlying ring buffer.
    fn generation(&self) -> Result<u64>;
}

/// Blocking byte-stream reader that tracks its position via a reader slot,
/// preventing the ring buffer from overwriting unread data.
///
/// Implements [`AsyncRead`] for raw byte-stream consumption. Each
/// `poll_read` returns bytes from a single complete frame (header +
/// payload) as they appear in the ring buffer. Use
/// [`FramedRead`](crate::io::framed::FramedRead) for frame-level
/// operations.
pub struct BlockingReader {
    region: ChannelRegion,
    pos: u64,
    reader_id: u32,
    terminated: bool,
    last_generation: u64,
    /// Buffered frame bytes from a read partially copied to the caller.
    pending_frame: Vec<u8>,
    /// Offset into `pending_frame` for the next copy.
    pending_offset: usize,
}

/// Non-blocking byte-stream reader that does not prevent buffer overwrite.
///
/// If writers overtake this reader, it reports [`Error::Overwritten`] and
/// resumes at the live tail. Implements [`AsyncRead`].
pub struct Reader {
    region: ChannelRegion,
    pos: u64,
    backpressure: ChannelBackpressure,
    terminated: bool,
    last_generation: u64,
    /// Buffered frame bytes from a read partially copied to the caller.
    pending_frame: Vec<u8>,
    /// Offset into `pending_frame` for the next copy.
    pending_offset: usize,
}

/// Weak (slotless) frame reader that always reads the newest available
/// frames without ever backpressuring writers.
///
/// `WeakReader` is the formalised "weak reader": unlike [`BlockingReader`]
/// it allocates no reader slot, so writers can always advance — a full ring
/// overwrites the oldest unread frames rather than blocking. Each [`drain`]
/// snaps forward to the newest window when the writer has overtaken it, then
/// returns every committed frame up to the write tail, losing only the
/// records that were overwritten.
///
/// This is the read handle for log drains and other fire-and-forget
/// consumers where losing old records is acceptable but blocking a producer
/// is not.
///
/// [`drain`]: WeakReader::drain
pub struct WeakReader {
    region: ChannelRegion,
    pos: u64,
}

impl BlockingReader {
    /// Creates a new blocking reader at `start_pos` with the given `reader_id`.
    pub fn new(region: ChannelRegion, start_pos: u64, reader_id: u32) -> Self {
        Self {
            region,
            pos: start_pos,
            reader_id,
            terminated: false,
            last_generation: 0,
            pending_frame: Vec::new(),
            pending_offset: 0,
        }
    }

    /// Returns the current generation counter from the underlying ring buffer.
    pub fn generation(&self) -> Result<u64> {
        self.region.load_generation()
    }

    /// Returns the current read position.
    pub fn position(&self) -> u64 {
        self.pos
    }

    /// Returns a reference to the underlying channel region.
    pub fn region(&self) -> &ChannelRegion {
        &self.region
    }

    /// Downgrade this blocking reader to a non-blocking reader, releasing the reader slot.
    pub fn downgrade(mut self) -> Reader {
        drop(self.region.release_reader_slot(self.reader_id));
        let reader = Reader {
            region: self.region.clone(),
            pos: self.pos,
            backpressure: ChannelBackpressure::Park,
            terminated: self.terminated,
            last_generation: self.last_generation,
            pending_frame: std::mem::take(&mut self.pending_frame),
            pending_offset: self.pending_offset,
        };
        self.terminated = true; // prevent Drop from releasing again
        reader
    }

    /// Close this reader and release its reader slot.
    pub fn close(&mut self) {
        if !self.terminated {
            drop(self.region.release_reader_slot(self.reader_id));
            self.terminated = true;
        }
    }

    fn advance(&mut self, frame_size: u64) -> Result<()> {
        self.pos = self.pos.checked_add(frame_size).ok_or_else(|| {
            Error::InvalidFrame(format!(
                "advance position overflow: {} + {frame_size}",
                self.pos
            ))
        })?;
        let result = self.region.update_reader_slot(self.reader_id, self.pos);
        if result.is_ok() {
            // Consuming frees ring capacity, which may unblock writers parked
            // on a full Park ring via generation-wait. The generation counter
            // only bumps on writes, so notify waiters directly without
            // bumping: spurious wakeups are benign (waiters re-poll and
            // re-register), but skipping this notification deadlocks a sole
            // producer waiting for capacity that only readers can free.
            selium_memory::wake_generation_waiters(self.region.region_id(), u64::MAX);
        }
        result
    }
}

impl HasGeneration for BlockingReader {
    fn generation(&self) -> Result<u64> {
        self.region.load_generation()
    }
}

impl AsyncRead for BlockingReader {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        // Drain any buffered frame bytes from a previous partial read.
        if self.pending_offset < self.pending_frame.len() {
            let remaining = &self.pending_frame[self.pending_offset..];
            let to_copy = remaining.len().min(buf.remaining());
            buf.put_slice(&remaining[..to_copy]);
            self.pending_offset += to_copy;
            if self.pending_offset >= self.pending_frame.len() {
                self.pending_frame.clear();
                self.pending_offset = 0;
            }
            return Poll::Ready(Ok(()));
        }

        if self.terminated {
            return Poll::Ready(Ok(())); // EOF
        }

        let capacity = self.region.capacity();
        let mask = capacity - 1;

        let tail = match self.region.read_next_tail() {
            Ok(t) => t,
            Err(e) => return Poll::Ready(Err(std::io::Error::other(e))),
        };

        if self.pos >= tail {
            // No data. Check if writers are still connected.
            match self.region.load_writer_count() {
                Ok(0) => return Poll::Ready(Ok(())), // EOF
                Ok(_) => {
                    // Register for generation wake before going to sleep.
                    let cur_gen = self.region.load_generation().unwrap_or(0);
                    if !selium_memory::register_generation_wait(
                        self.region.region_id(),
                        cur_gen,
                        cx.waker(),
                    ) {
                        cx.waker().wake_by_ref();
                        return Poll::Pending;
                    }
                    // Check-after-register: a writer may have committed —
                    // or the last writer closed — between our empty/EOF
                    // check and the registration. Re-arm so we are re-polled
                    // and observe the data or the EOF.
                    if self.region.read_next_tail().map(|t| self.pos < t) == Ok(true)
                        || self.region.load_generation().ok() != Some(cur_gen)
                        || self.region.load_writer_count().ok() == Some(0)
                    {
                        cx.waker().wake_by_ref();
                    }
                    return Poll::Pending;
                }
                Err(e) => return Poll::Ready(Err(std::io::Error::other(e))),
            }
        }

        // Check for overwrite using generation counter delta.
        let current_gen = match self.region.load_generation() {
            Ok(g) => g,
            Err(e) => return Poll::Ready(Err(std::io::Error::other(e))),
        };
        let delta = current_gen.wrapping_sub(self.last_generation);
        if delta > capacity {
            return Poll::Ready(Err(std::io::Error::other(Error::Overwritten)));
        }

        fence(Ordering::Acquire);

        let header = match read_header(&self.region, self.pos, mask) {
            Ok(h) => h,
            Err(e) => return Poll::Ready(Err(std::io::Error::other(e))),
        };

        if !header.is_ready() {
            // Frame not yet committed; register for generation wake.
            let cur_gen = self.region.load_generation().unwrap_or(0);
            if !selium_memory::register_generation_wait(
                self.region.region_id(),
                cur_gen,
                cx.waker(),
            ) {
                cx.waker().wake_by_ref();
                return Poll::Pending;
            }
            // Check-after-register: the frame may have been committed
            // between our readiness check and the registration.
            if self.region.load_generation().ok() != Some(cur_gen) {
                cx.waker().wake_by_ref();
            }
            return Poll::Pending;
        }

        let frame_size = header.frame_size();
        if frame_size > capacity {
            return Poll::Ready(Err(std::io::Error::other(Error::InvalidFrame(format!(
                "frame size {frame_size} exceeds capacity {capacity}"
            )))));
        }

        // Read the full frame (header + payload) from the ring buffer.
        let frame_bytes = match read_raw(&self.region, self.pos, frame_size, mask) {
            Ok(b) => b,
            Err(e) => return Poll::Ready(Err(std::io::Error::other(e))),
        };

        if let Err(e) = self.advance(frame_size) {
            return Poll::Ready(Err(std::io::Error::other(e)));
        }
        self.last_generation = self.region.load_generation().unwrap_or(0);

        let to_copy = frame_bytes.len().min(buf.remaining());
        buf.put_slice(&frame_bytes[..to_copy]);

        if to_copy < frame_bytes.len() {
            self.pending_frame = frame_bytes;
            self.pending_offset = to_copy;
        }

        Poll::Ready(Ok(()))
    }
}

impl Drop for BlockingReader {
    fn drop(&mut self) {
        self.close();
    }
}

impl Reader {
    pub(crate) fn new(
        region: ChannelRegion,
        start_pos: u64,
        backpressure: ChannelBackpressure,
    ) -> Self {
        Self {
            region,
            pos: start_pos,
            backpressure,
            terminated: false,
            last_generation: 0,
            pending_frame: Vec::new(),
            pending_offset: 0,
        }
    }

    /// Returns the current generation counter from the underlying ring buffer.
    pub fn generation(&self) -> Result<u64> {
        self.region.load_generation()
    }

    /// Returns the current read position.
    pub fn position(&self) -> u64 {
        self.pos
    }

    /// Returns a reference to the underlying channel region.
    pub fn region(&self) -> &ChannelRegion {
        &self.region
    }

    /// Returns the backpressure strategy for this reader's channel.
    pub fn backpressure(&self) -> ChannelBackpressure {
        self.backpressure
    }

    /// Upgrade this non-blocking reader to a blocking reader, allocating a reader slot.
    ///
    /// On Park channels, writers backpressure when the blocking reader is slow.
    /// On Drop channels, writers silently drop data when the blocking reader is slow.
    pub fn upgrade(self) -> Result<BlockingReader> {
        let reader_id = self.region.allocate_reader_slot(self.pos)?;
        let mut reader = BlockingReader::new(self.region, self.pos, reader_id);
        reader.last_generation = self.last_generation;
        Ok(reader)
    }
}

impl HasGeneration for Reader {
    fn generation(&self) -> Result<u64> {
        self.region.load_generation()
    }
}

impl AsyncRead for Reader {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        // Drain any buffered frame bytes from a previous partial read.
        if self.pending_offset < self.pending_frame.len() {
            let remaining = &self.pending_frame[self.pending_offset..];
            let to_copy = remaining.len().min(buf.remaining());
            buf.put_slice(&remaining[..to_copy]);
            self.pending_offset += to_copy;
            if self.pending_offset >= self.pending_frame.len() {
                self.pending_frame.clear();
                self.pending_offset = 0;
            }
            return Poll::Ready(Ok(()));
        }

        if self.terminated {
            return Poll::Ready(Ok(())); // EOF
        }

        let capacity = self.region.capacity();
        let mask = capacity - 1;

        let tail = match self.region.read_next_tail() {
            Ok(t) => t,
            Err(e) => return Poll::Ready(Err(std::io::Error::other(e))),
        };

        if self.pos >= tail {
            match self.region.load_writer_count() {
                Ok(0) => return Poll::Ready(Ok(())), // EOF
                Ok(_) => {
                    let cur_gen = self.region.load_generation().unwrap_or(0);
                    selium_memory::register_generation_wait(
                        self.region.region_id(),
                        cur_gen,
                        cx.waker(),
                    );
                    // Check-after-register: a writer may have committed — or
                    // the last writer closed — between our empty/EOF check
                    // and the registration, leaving this registration
                    // permanently stale. Re-arm so we are re-polled and
                    // observe the data or the EOF.
                    if self.region.read_next_tail().map(|t| self.pos < t) == Ok(true)
                        || self.region.load_generation().ok() != Some(cur_gen)
                        || self.region.load_writer_count().ok() == Some(0)
                    {
                        cx.waker().wake_by_ref();
                    }
                    return Poll::Pending;
                }
                Err(e) => return Poll::Ready(Err(std::io::Error::other(e))),
            }
        }

        // Non-blocking reader: if overtaken, jump to tail and report error.
        if self.pos.wrapping_add(capacity) < tail {
            self.pos = tail;
            return Poll::Ready(Err(std::io::Error::other(Error::Overwritten)));
        }

        fence(Ordering::Acquire);

        let header = match read_header(&self.region, self.pos, mask) {
            Ok(h) => h,
            Err(e) => return Poll::Ready(Err(std::io::Error::other(e))),
        };

        if !header.is_ready() {
            let cur_gen = self.region.load_generation().unwrap_or(0);
            if !selium_memory::register_generation_wait(
                self.region.region_id(),
                cur_gen,
                cx.waker(),
            ) {
                cx.waker().wake_by_ref();
                return Poll::Pending;
            }
            // Check-after-register: the frame may have been committed
            // between our readiness check and the registration.
            if self.region.load_generation().ok() != Some(cur_gen) {
                cx.waker().wake_by_ref();
            }
            return Poll::Pending;
        }

        let frame_size = header.frame_size();
        if frame_size > capacity {
            return Poll::Ready(Err(std::io::Error::other(Error::InvalidFrame(format!(
                "frame size {frame_size} exceeds capacity {capacity}"
            )))));
        }

        // Read the full frame (header + payload) from the ring buffer.
        let frame_bytes = match read_raw(&self.region, self.pos, frame_size, mask) {
            Ok(b) => b,
            Err(e) => return Poll::Ready(Err(std::io::Error::other(e))),
        };

        self.pos = match self.pos.checked_add(frame_size) {
            Some(p) => p,
            None => {
                return Poll::Ready(Err(std::io::Error::other(Error::InvalidFrame(format!(
                    "reader position overflow: {} + {frame_size}",
                    self.pos
                )))));
            }
        };
        self.last_generation = self.region.load_generation().unwrap_or(0);

        let to_copy = frame_bytes.len().min(buf.remaining());
        buf.put_slice(&frame_bytes[..to_copy]);

        if to_copy < frame_bytes.len() {
            self.pending_frame = frame_bytes;
            self.pending_offset = to_copy;
        }

        Poll::Ready(Ok(()))
    }
}

impl WeakReader {
    /// Creates a weak reader at `start_pos`.
    ///
    /// `start_pos` is caller-chosen: `0` reads everything still present in
    /// the ring from its beginning; the live write tail skips history.
    pub(crate) fn new(region: ChannelRegion, start_pos: u64) -> Self {
        Self {
            region,
            pos: start_pos,
        }
    }

    /// Returns the current read position.
    pub fn position(&self) -> u64 {
        self.pos
    }

    /// Returns a reference to the underlying channel region.
    pub fn region(&self) -> &ChannelRegion {
        &self.region
    }

    /// Drains committed frames available since the last drain, snapping past
    /// any frames that were overwritten. Returns frame payloads (without
    /// headers).
    pub fn drain(&mut self) -> Result<Vec<Vec<u8>>> {
        let (frames, pos) =
            crate::layout::drain_frames(self.region.backend(), self.pos, self.region.capacity())?;
        self.pos = pos;
        Ok(frames)
    }
}

pub(crate) fn read_raw(region: &ChannelRegion, pos: u64, len: u64, mask: u64) -> Result<Vec<u8>> {
    if len == 0 {
        return Ok(Vec::new());
    }
    if len > region.capacity() {
        return Err(Error::InvalidFrame(format!(
            "read length {len} exceeds ring capacity {}",
            region.capacity()
        )));
    }

    let raw_pos = region.data_offset() + (pos & mask);
    let ring_end = region.data_offset() + region.capacity();
    let wrap = raw_pos + len > ring_end;
    if !wrap {
        return region.mapping().read(raw_pos, len).map_err(Error::from);
    }

    let tail_len = ring_end.saturating_sub(raw_pos);
    let head_len = len - tail_len;
    let mut buf = Vec::with_capacity(len as usize);
    if tail_len > 0 {
        let part = region.mapping().read(raw_pos, tail_len)?;
        buf.extend_from_slice(&part);
    }
    if head_len > 0 {
        let part = region.mapping().read(region.data_offset(), head_len)?;
        buf.extend_from_slice(&part);
    }
    Ok(buf)
}

fn read_header(region: &ChannelRegion, pos: u64, mask: u64) -> Result<FrameHeader> {
    let header_bytes = read_raw(region, pos, FrameHeader::ENCODED_SIZE as u64, mask)?;
    FrameHeader::decode(&header_bytes).map_err(|e| match e {
        selium_memory::MemoryError::CorruptedHeader => Error::TornFrame { position: pos },
        other => Error::InvalidFrame(other.to_string()),
    })
}

#[cfg(test)]
mod tests {
    use crate::{Channel, ChannelBackpressure, ring_buf::RingBuf};
    use selium_memory::FrameHeader;

    fn write_frame(ring: &RingBuf, payload: &[u8]) {
        let required = FrameHeader::ENCODED_SIZE as u64 + payload.len() as u64;
        let pos = ring.reserve(required).expect("reserve");
        ring.write_frame(pos, payload, 0, 0).expect("write");
    }

    #[test]
    fn weak_reader_drains_all_committed_frames() {
        let channel = Channel::create(128, ChannelBackpressure::Park).expect("create");
        write_frame(channel.ring(), b"one");
        write_frame(channel.ring(), b"two");

        let mut reader = channel.weak_reader();
        let drained = reader.drain().expect("drain");

        assert_eq!(drained, vec![b"one".to_vec(), b"two".to_vec()]);
        // No new frames: a second drain is empty and advances nothing.
        assert!(reader.drain().expect("drain again").is_empty());
    }

    #[test]
    fn weak_reader_never_backpressures_writers() {
        // A ring far too small to hold every frame: the weak reader must not
        // hold the write tail open — every write succeeds through overwrite.
        let channel = Channel::create(128, ChannelBackpressure::Park).expect("create");
        for i in 0u8..8 {
            write_frame(channel.ring(), &[i; 64]);
        }
        // All eight frames were committed: the ring wrapped several times
        // without a reader slot ever blocking a write.
        assert_eq!(
            channel.ring().read_next_tail().expect("tail"),
            (FrameHeader::ENCODED_SIZE as u64 + 64) * 8
        );
        // Draining is best-effort and must not error, even after overwrite.
        let mut reader = channel.weak_reader();
        drop(reader.drain());
    }

    #[test]
    fn weak_reader_resyncs_after_torn_snap() {
        // Four 80-byte frames (16-byte header + 64-byte payload) in a 128-byte
        // ring wrap twice. A fresh weak reader snaps to `next_tail - capacity`
        // = 192, which lands inside frame C's payload (spans 176..240). The
        // checksum detects the torn read and the drain resyncs to frame D's
        // boundary instead of misdelivering garbage or erroring.
        let channel = Channel::create(128, ChannelBackpressure::Park).expect("create");
        for i in 0u8..4 {
            write_frame(channel.ring(), &[i; 64]);
        }

        let mut reader = channel.weak_reader();
        let drained = reader.drain().expect("drain");

        // Only the newest fully-intact frame survives the overwrite; exactly
        // that frame is delivered, from a true boundary.
        assert_eq!(drained, vec![vec![3u8; 64]]);
        assert_eq!(
            reader.position(),
            (FrameHeader::ENCODED_SIZE as u64 + 64) * 4
        );
    }
}
