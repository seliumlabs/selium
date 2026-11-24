#![expect(
    clippy::indexing_slicing,
    reason = "pending_frame offsets are bounds-checked before access"
)]

use std::{
    pin::Pin,
    task::{Context, Poll},
};

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

/// How a blocking reader interprets an empty ring that has no writers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeerLifecycle {
    /// The peer's existence is already established, so an empty ring with no
    /// writers is ordinary EOF.
    ///
    /// This is the default for rendezvous recipients and for every legacy
    /// many-to-many channel reader, whose membership policy is unchanged.
    Attached,
    /// No peer has attached yet: an empty ring with no writers is pending
    /// attachment, not closure.
    ///
    /// The snapshot distinguishes initial absence from a peer that attached
    /// and withdrew before the reader's first poll: any generation movement
    /// (writer attachment, a committed frame, peer withdrawal, or an explicit
    /// departure mark) establishes that a peer existed.
    AwaitingFirstPeer {
        /// Generation observed when the reader was created.
        initial_generation: u64,
    },
}

/// Blocking byte-stream reader that tracks its position via a reader slot,
/// preventing the ring buffer from overwriting unread data.
///
/// Implements [`AsyncRead`] for raw byte-stream consumption. Each
/// `poll_read` returns bytes from a single complete frame (header +
/// payload) as they appear in the ring buffer. Use
/// [`FramedRead`](crate::io::framed::FramedRead) for frame-level
/// operations.
///
/// A reader created with [`PeerLifecycle::AwaitingFirstPeer`] waits for its
/// point-to-point peer instead of reporting EOF before that peer ever
/// attaches. Committed frames are always drained before EOF is reported.
pub struct BlockingReader {
    region: ChannelRegion,
    pos: u64,
    reader_id: u32,
    terminated: bool,
    last_generation: u64,
    lifecycle: PeerLifecycle,
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
    ///
    /// The reader treats its peer as already attached: an empty ring with no
    /// writers is EOF. Use [`Self::new_awaiting_peer`] for a point-to-point
    /// reader that may be created before its remote peer attaches.
    pub fn new(region: ChannelRegion, start_pos: u64, reader_id: u32) -> Self {
        Self {
            region,
            pos: start_pos,
            reader_id,
            terminated: false,
            last_generation: 0,
            lifecycle: PeerLifecycle::Attached,
            pending_frame: Vec::new(),
            pending_offset: 0,
        }
    }

    /// Creates a blocking reader that waits for its first peer.
    ///
    /// Snapshots the ring generation at construction so that a peer which
    /// attaches and withdraws before the first poll is still observed as
    /// closure rather than initial absence (see [`PeerLifecycle`]).
    pub fn new_awaiting_peer(
        region: ChannelRegion,
        start_pos: u64,
        reader_id: u32,
    ) -> Result<Self> {
        let initial_generation = region.load_generation()?;
        // Activity that happened before this reader existed is already
        // evidence of a peer: an attached writer, committed data (a replaying
        // reader), or an explicit departure mark.
        let already_active = region.load_writer_count()? > 0
            || start_pos < region.read_next_tail()?
            || region.peer_closed()?;
        let lifecycle = if already_active {
            PeerLifecycle::Attached
        } else {
            PeerLifecycle::AwaitingFirstPeer { initial_generation }
        };
        Ok(Self {
            region,
            pos: start_pos,
            reader_id,
            terminated: false,
            last_generation: 0,
            lifecycle,
            pending_frame: Vec::new(),
            pending_offset: 0,
        })
    }

    /// Returns the reader's peer lifecycle.
    pub fn lifecycle(&self) -> PeerLifecycle {
        self.lifecycle
    }

    /// Reports whether the peer has closed, using the same lifecycle rules as
    /// [`AsyncRead`].
    ///
    /// An awaiting-first-peer reader reports `false` while no peer activity
    /// has ever been observed, so a transport cannot disagree with its own
    /// read half about closure.
    pub fn peer_closed(&self) -> Result<bool> {
        if self.region.peer_closed()? {
            return Ok(true);
        }
        let writer_count = self.region.load_writer_count()?;
        match self.lifecycle {
            PeerLifecycle::Attached => Ok(writer_count == 0),
            PeerLifecycle::AwaitingFirstPeer { initial_generation } => {
                if self.region.load_generation()? == initial_generation {
                    // No attachment, frame, withdrawal, or departure mark yet.
                    Ok(false)
                } else {
                    Ok(writer_count == 0)
                }
            }
        }
    }

    /// Registers for a generation wake after finding no data.
    ///
    /// Re-checks the empty/EOF/lifecycle conditions after registering so a
    /// transition that landed in between re-arms the task instead of being
    /// lost.
    fn park_for_generation(&self, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        let cur_gen = self.region.load_generation().unwrap_or(0);
        if !selium_memory::register_generation_wait(self.region.region_id(), cur_gen, cx.waker()) {
            cx.waker().wake_by_ref();
            return Poll::Pending;
        }
        // Check-after-register: a writer may have committed — or the last
        // writer closed, or a peer attached — between our empty/EOF check and
        // the registration. Re-arm so we are re-polled and observe it.
        let eof_now = match self.lifecycle {
            PeerLifecycle::Attached => self.region.load_writer_count().ok() == Some(0),
            // A zero writer count is still pending attachment here, so it is
            // not a reason to re-poll; generation movement covers the
            // attach/close transitions.
            PeerLifecycle::AwaitingFirstPeer { .. } => false,
        };
        if self.region.read_next_tail().map(|t| self.pos < t) == Ok(true)
            || self.region.load_generation().ok() != Some(cur_gen)
            || eof_now
        {
            cx.waker().wake_by_ref();
        }
        Poll::Pending
    }

    /// Records peer activity for an awaiting-first-peer reader.
    ///
    /// Returns whether a peer is known to exist. Writer presence, generation
    /// movement, committed data, and explicit departure all count as activity
    /// and permanently move the reader to [`PeerLifecycle::Attached`].
    fn observe_peer_activity(&mut self) -> Result<bool> {
        let PeerLifecycle::AwaitingFirstPeer { initial_generation } = self.lifecycle else {
            return Ok(true);
        };
        let active = self.region.load_writer_count()? > 0
            || self.pos < self.region.read_next_tail()?
            || self.region.load_generation()? != initial_generation
            || self.region.peer_closed()?;
        if active {
            self.lifecycle = PeerLifecycle::Attached;
        }
        Ok(active)
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
                Ok(0) => {
                    // An awaiting-first-peer reader must not mistake initial
                    // absence for closure; once activity is recorded this is
                    // ordinary EOF.
                    match self.observe_peer_activity() {
                        Ok(true) => return Poll::Ready(Ok(())), // EOF
                        Ok(false) => {}
                        Err(e) => return Poll::Ready(Err(std::io::Error::other(e))),
                    }
                    return self.park_for_generation(cx);
                }
                // A live writer is peer activity: record it so an awaiting
                // reader stops waiting for a first peer.
                Ok(_) => {
                    if let Err(e) = self.observe_peer_activity() {
                        return Poll::Ready(Err(std::io::Error::other(e)));
                    }
                    return self.park_for_generation(cx);
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

        let frame = match crate::layout::read_frame(
            self.region.mapping().backend(),
            self.pos,
            mask,
            capacity,
        ) {
            Ok(frame) => frame,
            Err(e) => return Poll::Ready(Err(std::io::Error::other(e))),
        };
        let Some((header, payload)) = frame else {
            park_uncommitted(&self.region, cx);
            return Poll::Pending;
        };

        let frame_size = header.frame_size();
        let mut frame_bytes = header.encode().to_vec();
        frame_bytes.extend_from_slice(&payload);

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

        let frame = match crate::layout::read_frame(
            self.region.mapping().backend(),
            self.pos,
            mask,
            capacity,
        ) {
            Ok(frame) => frame,
            Err(e) => return Poll::Ready(Err(std::io::Error::other(e))),
        };
        let Some((header, payload)) = frame else {
            park_uncommitted(&self.region, cx);
            return Poll::Pending;
        };

        let frame_size = header.frame_size();
        let mut frame_bytes = header.encode().to_vec();
        frame_bytes.extend_from_slice(&payload);

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

fn park_uncommitted(region: &ChannelRegion, cx: &mut Context<'_>) {
    let generation = region.load_generation().unwrap_or(0);
    if !selium_memory::register_generation_wait(region.region_id(), generation, cx.waker()) {
        cx.waker().wake_by_ref();
        return;
    }
    // Check-after-register: the frame may have committed between the
    // canonical readiness check and wait registration.
    if region.load_generation().ok() != Some(generation) {
        cx.waker().wake_by_ref();
    }
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
