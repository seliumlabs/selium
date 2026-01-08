#![deny(missing_docs)]
//! In-memory asynchronous channel implementation with configurable backpressure.

use std::{cell::UnsafeCell, collections::VecDeque, io, sync::Arc, task::Waker};

#[cfg(feature = "loom")]
use loom::sync::{
    Mutex, RwLock,
    atomic::{AtomicBool, AtomicU64, Ordering},
};
#[cfg(not(feature = "loom"))]
use std::sync::{
    Mutex, RwLock,
    atomic::{AtomicBool, AtomicU64, Ordering},
};

use stable_vec::StableVec;
use thiserror::Error;
use tracing::{Span, debug, field::Empty, instrument};

mod driver;
mod id_factory;
mod reader;
mod writer;

pub use driver::{ChannelDriver, ChannelStrongIoDriver, ChannelWeakIoDriver};
pub use reader::{Reader, StrongReader, WeakReader};
pub use writer::{StrongWriter, WeakWriter, Writer};

use crate::id_factory::{Id, IdFactory};

type Result<T> = std::result::Result<T, ChannelError>;

/// Backpressure policy for writers when the buffer is full.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backpressure {
    /// Writers park (Pending) until space is available.
    Park,
    /// Writers drop data when no space is available (return Ok(0)).
    Drop,
}

/// Intermediate storage backing every [`Channel`].
///
/// # Safety
/// The ring buffer maintains monotonically increasing cursors and is written to
/// exclusively through the channel’s slice reservation helpers. Those helpers
/// ensure writers and readers never alias; the raw pointer copies inside
/// [`RingBuffer::read`] and [`RingBuffer::write`] therefore rely on the caller
/// upholding those preconditions. All public API entry points funnel through
/// the safe [`Channel`] methods, so additional unsafe usage must preserve the
/// same guarantees.
struct RingBuffer {
    /// Internal storage for the buffer
    buf: UnsafeCell<Box<[u8]>>,
    /// Size of `buf`
    size: usize,
    /// Used to convert an incremental position into a valid index of `buf`
    mask: u64,
}

#[derive(Clone)]
struct FrameMeta {
    start: u64,
    len: u64,
    writer_id: u16,
}

/// In-memory, asynchronous, many-to-many byte channel with explicit backpressure semantics.
///
/// Writers and readers operate on a shared ring buffer while maintaining their own
/// monotonic cursors. Reserving slices (writers) or releasing read positions (readers)
/// is the only way to advance those cursors, ensuring deterministic progress and
/// preventing aliasing of the underlying buffer.
pub struct Channel {
    /// Underlying data store
    buf: RingBuffer,
    /// Queue of Wakers to be woken once the given position is reached
    queue: Mutex<Vec<(u64, Waker)>>,
    /// Position of each `StrongReader`
    heads: RwLock<StableVec<Arc<AtomicU64>>>,
    /// Position of each `Writer`
    tails: RwLock<StableVec<Arc<AtomicU64>>>,
    /// Writer Id generator
    idf: Arc<IdFactory>,
    /// Frame metadata keyed by start position.
    frames: Mutex<VecDeque<FrameMeta>>,
    /// Maximum position for removed tails.
    ///
    /// This is needed when `tails` is empty but we still need to know where the tail is.
    tail_cache: AtomicU64,
    /// Next position that a `Writer` can write to
    next_tail: AtomicU64,
    /// Whether the channel is terminated
    terminated: AtomicBool,
    /// Whether the channel is draining
    draining: AtomicBool,
    /// Backpressure behavior when writers encounter a full buffer
    backpressure: Backpressure,
    /// Tracing span for instrumentation
    span: Span,
}

/// Channel operation errors.
#[derive(Error, Debug, PartialEq)]
pub enum ChannelError {
    /// Reader was too slow and data was dropped; value is number of lost bytes.
    #[error("Reader was too slow and got left behind")]
    ReaderBehind(u64),
    /// User asked channel to terminate whilst draining.
    #[error("Cannot terminate channel that is draining")]
    TerminateDraining,
    /// User asked channel to drain after terminating.
    #[error("Cannot drain a terminated channel")]
    DrainTerminated,
    /// Generic IO error surfaced by channel operations.
    #[error("io error: {0}")]
    Io(String),
}

impl From<io::Error> for ChannelError {
    fn from(value: io::Error) -> Self {
        Self::Io(value.to_string())
    }
}

impl RingBuffer {
    fn new(mut size: usize) -> Self {
        size = size.next_power_of_two();
        // Allocate a buffer with an initialized length of `size` bytes.
        // Using `with_capacity` would create a zero-length boxed slice,
        // leading to out-of-bounds pointer arithmetic and UB during reads/writes.
        let buf: Vec<u8> = vec![0u8; size];

        Self {
            buf: UnsafeCell::new(buf.into_boxed_slice()),
            size,
            mask: (size - 1) as u64,
        }
    }

    /// Reads `self.buf` into `dst` until full.
    ///
    /// # Safety
    /// The caller must ensure no concurrent writer is touching the overlapping region.
    ///
    /// # Panics
    ///
    /// This function panics if `dst` is larger than `self.buf`.
    unsafe fn read(&self, dst: &mut [u8], pos: u64) {
        let dstlen = dst.len().min(self.size);

        // Convert position to array index
        let idx: usize = (pos & self.mask)
            .try_into()
            .expect("pointer size less than 64b");

        let taillen = dstlen.min(self.size - idx);
        let headlen = idx.min(dstlen - taillen);

        // Get a raw pointer to the start of the underlying byte slice
        let src = unsafe { (&mut *self.buf.get()).as_mut_ptr() };
        let dst = dst.as_mut_ptr();

        // Write tail to dst
        unsafe {
            src.add(idx).copy_to_nonoverlapping(dst, taillen);
        }

        if headlen > 0 {
            // Write head to dst
            unsafe {
                src.copy_to_nonoverlapping(dst.add(taillen), headlen);
            }
        }
    }

    /// Writes `src` into `self.buf` until exhausted.
    ///
    /// # Safety
    /// The caller must ensure no concurrent reader consumes the overlapping region.
    ///
    /// # Panics
    ///
    /// This function panics if `src` is larger than `self.buf`.
    unsafe fn write(&self, src: &[u8], pos: u64) {
        let srclen = src.len().min(self.size);

        // Convert position to array index
        let idx: usize = (pos & self.mask)
            .try_into()
            .expect("pointer size less than 64b");

        let taillen = srclen.min(self.size - idx);
        let headlen = idx.min(srclen - taillen);

        let src = src.as_ptr();
        // Get a raw pointer to the start of the underlying byte slice
        let dst = unsafe { (&mut *self.buf.get()).as_mut_ptr() };

        // Write tail to dst
        unsafe {
            src.copy_to_nonoverlapping(dst.add(idx), taillen);
        }

        if headlen > 0 {
            // Write head to dst
            unsafe {
                src.add(taillen).copy_to_nonoverlapping(dst, headlen);
            }
        }
    }
}

unsafe impl Sync for RingBuffer {}

impl Channel {
    /// Create a channel with the provided capacity in bytes.
    pub fn new(size: usize) -> Arc<Self> {
        Self::with_parameters(size, Backpressure::Park)
    }

    /// Create a channel with the given parameters.
    #[instrument(name = "Channel", skip_all, fields(ptr = Empty))]
    pub fn with_parameters(size: usize, backpressure: Backpressure) -> Arc<Self> {
        let this = Arc::new(Self {
            buf: RingBuffer::new(size),
            queue: Mutex::new(Vec::new()),
            heads: RwLock::new(StableVec::new()),
            tails: RwLock::new(StableVec::new()),
            idf: IdFactory::new(),
            frames: Mutex::new(VecDeque::new()),
            tail_cache: AtomicU64::new(0),
            next_tail: AtomicU64::new(0),
            terminated: AtomicBool::new(false),
            draining: AtomicBool::new(false),
            backpressure,
            span: Span::current(),
        });

        this.span
            .record("ptr", format_args!("{:p}", this.as_ref() as *const _));
        debug!("create channel");

        this
    }

    fn get_head(&self) -> Option<u64> {
        let heads = self
            .heads
            .read()
            .unwrap_or_else(|poison| poison.into_inner());
        self.get_head_locked(&heads)
    }

    fn get_head_locked(&self, heads: &StableVec<Arc<AtomicU64>>) -> Option<u64> {
        heads.iter().map(|(_, t)| t.load(Ordering::Acquire)).min()
    }

    fn get_tail(&self) -> u64 {
        let tails = self
            .tails
            .read()
            .unwrap_or_else(|poison| poison.into_inner());
        self.get_tail_locked(&tails)
    }

    fn get_tail_locked(&self, tails: &StableVec<Arc<AtomicU64>>) -> u64 {
        tails
            .iter()
            .map(|(_, t)| t.load(Ordering::Acquire))
            .min()
            .unwrap_or(self.tail_cache.load(Ordering::Acquire))
    }

    fn remove_head(&self, idx: usize) {
        self.heads
            .write()
            .unwrap_or_else(|poison| poison.into_inner())
            .remove(idx);
        self.prune_frames();
    }

    fn remove_tail(&self, idx: usize) {
        let mut tails = self
            .tails
            .write()
            .unwrap_or_else(|poison| poison.into_inner());
        if let Some(tail) = tails.remove(idx) {
            self.tail_cache
                .fetch_max(tail.load(Ordering::Acquire), Ordering::AcqRel);
        }
    }

    fn register_frame(&self, start: u64, len: u64, writer_id: u16) {
        let mut frames = self
            .frames
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        frames.push_back(FrameMeta {
            start,
            len,
            writer_id,
        });
    }

    fn frame_for(&self, pos: u64) -> Option<FrameMeta> {
        let frames = self
            .frames
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        frames.iter().find(|frame| frame.start == pos).cloned()
    }

    fn prune_frames(&self) {
        let Some(head) = self.get_head() else {
            return;
        };

        let mut frames = self
            .frames
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        while let Some(frame) = frames.front() {
            if frame.start + frame.len <= head {
                frames.pop_front();
            } else {
                break;
            }
        }
    }

    /// Remaining writable capacity before the tail would overrun the head.
    fn writable_size(&self, pos: u64) -> u64 {
        (self.buf.size as u64).saturating_sub(pos - self.get_head().unwrap_or(pos))
    }

    /// Copy `buf` into the ring buffer at logical position `pos` without advancing cursors.
    fn write(&self, pos: u64, buf: &[u8]) -> usize {
        // Calculate max buf length to prevent overrunning the head
        let len = (buf.len() as u64).min(self.writable_size(pos));

        // No point proceeding if no space
        if len == 0 {
            return 0;
        }

        let ulen: usize = len.try_into().expect("pointer size less than 64b");

        // Safety: we prevent overrunning the head position, however the caller must
        // ensure that they abide by the position and length provided by `reserve_slice()`.
        unsafe { self.buf.write(&buf[..ulen], pos) };

        ulen
    }

    /// Read into `buf`, returning an error if the cursor has been lapped by writers.
    fn read(&self, pos: u64, buf: &mut [u8]) -> Result<usize> {
        // Ensure the given head position hasn't been overwritten
        let tail = self.get_tail();
        if pos + (self.buf.size as u64) < tail {
            return Err(ChannelError::ReaderBehind(tail - self.buf.size as u64));
        }

        Ok(unsafe { self.read_unsafe(pos, buf) })
    }

    /// Read into `buf` without validating that `pos` has not been overwritten.
    ///
    /// # Safety
    /// The caller must ensure `pos` remains ahead of the oldest writer.
    unsafe fn read_unsafe(&self, pos: u64, buf: &mut [u8]) -> usize {
        // Calculate max buf length to prevent overrunning the tail
        let len = (buf.len() as u64).min(self.get_tail().saturating_sub(pos));

        // No point proceeding if no space
        if len == 0 {
            return 0;
        }

        let ulen: usize = len.try_into().expect("pointer size less than 64b");

        // Safety: we never allow `buf` to overrun the tail position, so races are
        // impossible unless the head position has been overrun. Head overruns are
        // impossible for strong readers, though this function is inappropriate for
        // weak readers.
        unsafe { self.buf.read(&mut buf[..ulen], pos) };

        ulen
    }

    /// Queue a `Waker` to be woken once `pos` has been reached by either the head or tail.
    ///
    /// Note: If the given position is less than the head position, it will never be woken.
    #[instrument(parent = &self.span, skip(self, waker))]
    fn enqueue(&self, pos: u64, waker: Waker) {
        debug!(pos, "channel enqueue");
        self.queue
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .push((pos, waker));
    }

    /// Wake any writers whose reserved spans are now safe to fill.
    #[instrument(parent = &self.span, skip(self))]
    fn schedule_writers(&self) {
        // If nothing to schedule, exit early
        if self
            .tails
            .read()
            .unwrap_or_else(|poison| poison.into_inner())
            .is_empty()
        {
            return;
        }

        let tail_pos = self.get_tail();
        let head_pos = self.get_head().unwrap_or(tail_pos);
        let mut queue = self
            .queue
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());

        debug!(
            queued = queue.len(),
            head_pos, tail_pos, "channel schedule_writers"
        );

        // Dequeue and wake each Waker in a writable position
        queue
            .extract_if(.., |(pos, _)| {
                let wake = *pos < (head_pos + self.buf.size as u64) && *pos >= tail_pos;
                if wake {
                    debug!(pos, "channel wake writer");
                }
                wake
            })
            .for_each(|(_, waker)| waker.wake());
    }

    /// Wake any readers that now have data available.
    #[instrument(parent = &self.span, skip(self))]
    fn schedule_readers(&self) {
        // If nothing to schedule, exit early
        if self
            .heads
            .read()
            .unwrap_or_else(|poison| poison.into_inner())
            .is_empty()
        {
            return;
        }

        let head_pos = self.get_head().unwrap_or(0);
        let tail_pos = self.get_tail();
        let mut queue = self
            .queue
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());

        debug!(
            queued = queue.len(),
            head_pos, tail_pos, "channel schedule_readers"
        );
        // Dequeue and wake each Waker in a readable position
        queue
            .extract_if(.., |(pos, _)| {
                let wake = *pos < tail_pos && *pos >= head_pos;
                if wake {
                    debug!(pos, "channel wake reader");
                }
                wake
            })
            .for_each(|(_, waker)| waker.wake());
    }

    /// Create a new writer to push bytes to the channel.
    ///
    /// # Concurrency
    /// Writers can work somewhat concurrently, given enough buffer space. This works by
    /// allowing multiple writers to reserve and write contiguous buffer "slices",
    /// provided that those slices do not overwrite any part of the buffer being consumed
    /// by a strong reader. Readers are not aware of slices and will read the buffer in
    /// the order that slices are written. If a writer hangs while writing a slice, no
    /// subsequent slices will be read.
    pub fn new_writer(self: &Arc<Channel>) -> Writer {
        Writer::Strong(self.new_strong_writer())
    }

    /// Create a new strong writer.
    pub fn new_strong_writer(self: &Arc<Channel>) -> StrongWriter {
        self.new_strong_writer_with_id(self.idf.generate())
    }

    fn new_strong_writer_with_id(self: &Arc<Channel>, id: Id) -> StrongWriter {
        let mut tails = self
            .tails
            .write()
            .unwrap_or_else(|poison| poison.into_inner());
        let pos = Arc::new(AtomicU64::new(self.get_tail_locked(&tails)));
        let pos_id = tails.push(pos.clone());
        drop(tails);

        StrongWriter::new(id, self.clone(), pos, Some(pos_id))
    }

    /// Create a new weak writer that acquires tail slots on demand.
    pub fn new_weak_writer(self: &Arc<Channel>) -> WeakWriter {
        WeakWriter::new(self.idf.generate(), self.clone())
    }

    /// Create a new reader that applies backpressure to writers if they are trying to
    /// overwrite the reader's log position.
    ///
    /// Note that the entire channel will be as slow as the *slowest* strong reader.
    pub fn new_strong_reader(self: &Arc<Channel>) -> StrongReader {
        let mut heads = self
            .heads
            .write()
            .unwrap_or_else(|poison| poison.into_inner());
        let pos = Arc::new(AtomicU64::new(
            self.get_head_locked(&heads)
                .unwrap_or_else(|| self.get_tail().saturating_sub(self.buf.size as u64)),
        ));
        let id = heads.push(pos.clone());
        drop(heads);

        StrongReader::new(self.clone(), pos, id)
    }

    /// Create a new reader that can be overtaken by writers if the reader is too slow.
    ///
    /// If you require a reader that cannot lose any data, create a `StrongReader` instead.
    pub fn new_weak_reader(self: &Arc<Channel>) -> WeakReader {
        WeakReader::new(
            self.clone(),
            self.get_head()
                .unwrap_or_else(|| self.get_tail().saturating_sub(self.buf.size as u64)),
        )
    }

    /// Reserve a tail position of given length to write to.
    ///
    /// # Caution!
    /// Failing to write the entire slice will result in *permanent backpressure.*
    pub fn reserve_slice(&self, len: u64) -> u64 {
        self.next_tail.fetch_add(len, Ordering::SeqCst)
    }

    /// Safely terminate this channel.
    ///
    /// This will cause any readers or writers to return `io::ErrorKind::ConnectionAborted`.
    #[instrument(parent = &self.span, skip(self))]
    pub fn terminate(&self) -> Result<()> {
        debug!("terminate channel");

        if self.draining.load(Ordering::Acquire) {
            return Err(ChannelError::TerminateDraining);
        }
        self.terminated.store(true, Ordering::Release);

        // Notify all queued wakers so they don't hang indefinitely
        self.queue
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .drain(..)
            .for_each(|(_, waker)| waker.wake());

        Ok(())
    }

    /// Start draining the channel.
    ///
    /// When draining, a channel doesn't accept any new frames from `Writer`s, and rejects reads
    /// once they catch up to the buffer tail.
    #[instrument(parent = &self.span, skip(self))]
    pub fn drain(&self) -> Result<()> {
        debug!("start draining channel");

        if self.terminated.load(Ordering::Acquire) {
            Err(ChannelError::DrainTerminated)
        } else {
            self.draining.store(true, Ordering::Release);
            Ok(())
        }
    }
}

impl Drop for Channel {
    fn drop(&mut self) {
        let _ = self.terminate();
    }
}

#[cfg(all(test, not(feature = "loom")))]
mod tests {
    use std::{
        pin::pin,
        task::{Context, Poll},
    };

    use futures::task::{noop_waker, noop_waker_ref};
    use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

    use super::*;

    #[test]
    fn drop_backpressure_returns_zero_when_full() {
        use std::task::Poll;

        use futures::task::{Context, noop_waker};

        let channel = Channel::with_parameters(2, Backpressure::Drop);
        let _reader_guard = channel.new_strong_reader();
        let waker = noop_waker();
        let mut cx = Context::from_waker(&waker);
        let writer = channel.new_writer();
        let mut pinned = pin!(writer);

        match pinned.as_mut().poll_write(&mut cx, &[1, 2]) {
            Poll::Ready(Ok(2)) => {}
            other => panic!("unexpected poll result: {:?}", other),
        }
        match pinned.as_mut().poll_write(&mut cx, &[3, 4]) {
            Poll::Ready(Ok(0)) => {}
            other => panic!("unexpected poll result: {:?}", other),
        }
    }

    #[test]
    fn drop_backpressure_leaves_existing_bytes_intact() {
        use std::task::Poll;

        use futures::task::{Context, noop_waker};

        let channel = Channel::with_parameters(2, Backpressure::Drop);
        let reader = channel.new_strong_reader();
        let waker = noop_waker();
        let mut cx = Context::from_waker(&waker);
        let writer = channel.new_writer();
        let mut pinned_writer = pin!(writer);

        match pinned_writer.as_mut().poll_write(&mut cx, &[7, 8]) {
            Poll::Ready(Ok(2)) => {}
            other => panic!("unexpected poll result: {:?}", other),
        }
        match pinned_writer.as_mut().poll_write(&mut cx, &[9, 10, 11]) {
            Poll::Ready(Ok(0)) => {}
            other => panic!("unexpected poll result: {:?}", other),
        }

        let mut pinned_reader = pin!(reader);
        let mut buf = [0u8; 2];
        let mut rb = ReadBuf::new(&mut buf);
        match pinned_reader.as_mut().poll_read(&mut cx, &mut rb) {
            Poll::Ready(Ok(())) if rb.filled().len() == 2 => {}
            other => panic!("unexpected poll result: {:?}", other),
        }
        assert_eq!(&buf, &[7, 8]);
    }

    #[test]
    fn ring_buffer_write_clamps_to_capacity() {
        let ring = RingBuffer::new(8);
        let data = vec![42u8; 32];
        unsafe { ring.write(&data, 0) };
        let mut buf = [0u8; 8];
        unsafe { ring.read(&mut buf, 0) };
        assert!(buf.iter().all(|b| *b == 42));
    }

    #[test]
    fn ring_buffer_read_large_destination_stays_within_bounds() {
        let ring = RingBuffer::new(8);
        let data: Vec<u8> = (0u8..8).collect();
        unsafe { ring.write(&data, 0) };
        let mut dst = [0u8; 16];
        unsafe { ring.read(&mut dst, 0) };
        assert_eq!(&dst[..8], &data[..]);
        assert!(dst[8..].iter().all(|b| *b == 0));
    }

    fn new_buf(size: u8) -> RingBuffer {
        let buf = RingBuffer::new(size as usize);
        unsafe {
            // Write test data directly into the underlying byte slice
            let dst = (&mut *buf.buf.get()).as_mut_ptr();
            dst.copy_from_nonoverlapping((0..size).collect::<Vec<_>>().as_ptr(), size as usize);
        }
        buf
    }

    #[test]
    fn test_ring_read_small() {
        let mut dst = [0; 2];

        let buf = new_buf(4);
        unsafe { buf.read(&mut dst, 1) };

        assert_eq!(dst, [1, 2]);
    }

    #[test]
    fn test_ring_read_large() {
        let mut dst = [0; 4];

        let buf = new_buf(4);
        unsafe { buf.read(&mut dst, 1) };

        assert_eq!(dst, [1, 2, 3, 0]);
    }

    #[test]
    fn test_ring_read_too_large() {
        let mut dst = [0; 5];

        let buf = new_buf(4);
        unsafe { buf.read(&mut dst, 1) };

        assert_eq!(&dst[..4], [1, 2, 3, 0].as_ref());
        assert_eq!(dst[4], 0);
    }

    #[test]
    fn test_ring_write_small() {
        let src = [4; 2];

        let buf = new_buf(4);
        unsafe { buf.write(&src, 1) };

        let mut dst = [0; 4];
        unsafe {
            let src = (&mut *buf.buf.get()).as_mut_ptr();
            src.copy_to_nonoverlapping(dst.as_mut_ptr(), 4)
        };
        assert_eq!(dst, [0, 4, 4, 3]);
    }

    #[test]
    fn test_ring_write_large() {
        let src = [4; 4];

        let buf = new_buf(4);
        unsafe { buf.write(&src, 1) };

        let mut dst = [0; 4];
        unsafe {
            let src = (&mut *buf.buf.get()).as_mut_ptr();
            src.copy_to_nonoverlapping(dst.as_mut_ptr(), 4)
        };
        assert_eq!(dst, [4, 4, 4, 4]);
    }

    #[test]
    fn test_ring_write_too_large() {
        let src = [4; 5];

        let buf = new_buf(4);
        unsafe { buf.write(&src, 1) };

        let mut dst = [0; 4];
        unsafe {
            let src = (&mut *buf.buf.get()).as_mut_ptr();
            src.copy_to_nonoverlapping(dst.as_mut_ptr(), 4)
        };
        assert_eq!(dst, [4, 4, 4, 4]);
    }

    #[test]
    fn test_channel_write() {
        let channel = Channel::new(4);
        assert_eq!(channel.write(0, &[]), 0);
        assert_eq!(channel.write(4, &[0; 3]), 3);
        assert_eq!(channel.write(1, &[0; 4]), 4);
        assert_eq!(channel.write(1, &[0; 5]), 4);

        channel
            .heads
            .write()
            .unwrap()
            .push(Arc::new(AtomicU64::new(1)));

        assert_eq!(channel.write(5, &[0; 3]), 0);
        assert_eq!(channel.write(3, &[0; 3]), 2);
    }

    #[test]
    fn test_channel_write_returns_zero_when_full() {
        let channel = Channel::new(4);
        channel
            .heads
            .write()
            .unwrap()
            .push(Arc::new(AtomicU64::new(0)));
        assert_eq!(channel.write(0, &[1, 2, 3, 4]), 4);
        assert_eq!(channel.write(4, &[9]), 0);
    }

    #[test]
    fn test_channel_read() {
        let channel = Channel::new(4);
        let mut buf = [0; 3];

        let tail = Arc::new(AtomicU64::new(2));
        channel.tails.write().unwrap().push(tail.clone());

        assert_eq!(channel.read(0, &mut buf).unwrap(), 2);

        tail.store(5, Ordering::Release);

        assert_eq!(
            channel.read(0, &mut buf).unwrap_err(),
            ChannelError::ReaderBehind(1)
        );
    }

    #[test]
    fn test_channel_read_unsafe() {
        let channel = Channel::new(4);
        let mut buf = [0; 3];

        assert_eq!(unsafe { channel.read_unsafe(0, &mut buf) }, 0);

        let tail = Arc::new(AtomicU64::new(2));
        channel.tails.write().unwrap().push(tail.clone());

        assert_eq!(channel.read(0, &mut buf).unwrap(), 2);

        tail.store(5, Ordering::Release);

        assert_eq!(channel.read(1, &mut buf).unwrap(), 3);
        assert_eq!(channel.read(3, &mut buf).unwrap(), 2);
    }

    #[test]
    fn test_channel_schedule_writers() {
        let channel = Channel::new(4);
        channel
            .tails
            .write()
            .unwrap()
            .push(Arc::new(AtomicU64::new(0)));

        let mut queue = channel.queue.lock().unwrap();
        queue.push((0, noop_waker()));
        queue.push((1, noop_waker()));
        queue.push((4, noop_waker()));
        drop(queue);

        channel.schedule_writers();

        let queue = channel.queue.lock().unwrap();
        assert_eq!(queue.len(), 1);
        assert_eq!(queue.first().unwrap().0, 4);
    }

    #[test]
    fn test_channel_schedule_readers() {
        let channel = Channel::new(4);
        channel
            .tails
            .write()
            .unwrap()
            .push(Arc::new(AtomicU64::new(2)));
        channel
            .heads
            .write()
            .unwrap()
            .push(Arc::new(AtomicU64::new(1)));

        let mut queue = channel.queue.lock().unwrap();
        queue.push((1, noop_waker()));
        queue.push((2, noop_waker()));
        drop(queue);

        channel.schedule_readers();

        let queue = channel.queue.lock().unwrap();
        assert_eq!(queue.len(), 1);
        assert_eq!(queue.first().unwrap().0, 2);
    }

    #[test]
    fn test_channel_new_writer() {
        let channel = Arc::new(Channel::new(4));
        let writer = channel.new_writer();
        assert_eq!(channel.tails.read().unwrap().num_elements(), 1);
        drop(writer);
        assert!(channel.tails.read().unwrap().is_empty());
    }

    #[test]
    fn test_channel_new_strong_reader() {
        let channel = Arc::new(Channel::new(4));
        channel
            .tails
            .write()
            .unwrap()
            .push(Arc::new(AtomicU64::new(5)));
        let reader = channel.new_strong_reader();
        assert_eq!(channel.heads.read().unwrap().num_elements(), 1);
        assert_eq!(reader.pos.load(Ordering::Acquire), 1);
        drop(reader);
        assert!(channel.heads.read().unwrap().is_empty());
    }

    #[test]
    fn test_writer_poll_write() {
        let mut cx = Context::from_waker(noop_waker_ref());
        let channel = Arc::new(Channel::new(4));
        channel
            .heads
            .write()
            .unwrap()
            .push(Arc::new(AtomicU64::new(0)));
        let mut writer = pin!(channel.new_strong_writer());

        assert!(matches!(
            writer.as_mut().poll_write(&mut cx, &[1, 2, 3]),
            Poll::Ready(Ok(3))
        ));
        assert_eq!(channel.next_tail.load(Ordering::Acquire), 3);
        assert_eq!(writer.pos.load(Ordering::Acquire), 3);
        assert_eq!(writer.rem, 0);

        assert!(matches!(
            writer.as_mut().poll_write(&mut cx, &[1, 2, 3]),
            Poll::Ready(Ok(1))
        ));
        assert_eq!(channel.next_tail.load(Ordering::Acquire), 6);
        assert_eq!(writer.pos.load(Ordering::Acquire), 4);
        assert_eq!(writer.rem, 2);

        assert!(writer.as_mut().poll_write(&mut cx, &[1, 2, 3]).is_pending());
    }

    #[test]
    fn test_writer_poll_strong_read() {
        let mut cx = Context::from_waker(noop_waker_ref());
        let channel = Arc::new(Channel::new(4));
        channel
            .tails
            .write()
            .unwrap()
            .push(Arc::new(AtomicU64::new(4)));
        unsafe {
            let dst = (&mut *channel.buf.buf.get()).as_mut_ptr();
            dst.copy_from_nonoverlapping((1..=4).collect::<Vec<_>>().as_ptr(), 4);
        }
        let mut reader = pin!(channel.new_strong_reader());

        let mut buf = [0; 3];
        let mut rb = ReadBuf::new(&mut buf);
        assert!(matches!(
            reader.as_mut().poll_read(&mut cx, &mut rb),
            Poll::Ready(Ok(()))
        ));
        assert_eq!(rb.filled().len(), 3);
        assert_eq!(reader.pos.load(Ordering::Acquire), 3);

        let mut buf = [0; 3];
        let mut rb = ReadBuf::new(&mut buf);
        assert!(matches!(
            reader.as_mut().poll_read(&mut cx, &mut rb),
            Poll::Ready(Ok(()))
        ));
        assert_eq!(rb.filled().len(), 1);
        assert_eq!(reader.pos.load(Ordering::Acquire), 4);

        let mut buf = [0; 3];
        let mut rb = ReadBuf::new(&mut buf);
        assert!(matches!(
            reader.as_mut().poll_read(&mut cx, &mut rb),
            Poll::Pending
        ));
    }

    #[test]
    fn test_writer_poll_weak_read() {
        let mut cx = Context::from_waker(noop_waker_ref());
        let channel = Arc::new(Channel::new(4));
        let tail_pos = Arc::new(AtomicU64::new(4));
        channel.tails.write().unwrap().push(tail_pos.clone());
        unsafe {
            let dst = (&mut *channel.buf.buf.get()).as_mut_ptr();
            dst.copy_from_nonoverlapping((1..=4).collect::<Vec<_>>().as_ptr(), 4);
        }
        let mut reader = pin!(channel.new_weak_reader());

        let mut buf = [0; 4];
        let mut rb = ReadBuf::new(&mut buf);
        assert!(matches!(
            reader.as_mut().poll_read(&mut cx, &mut rb),
            Poll::Ready(Ok(()))
        ));
        assert_eq!(rb.filled().len(), 4);
        assert_eq!(reader.pos, 4);

        let mut buf = [0; 1];
        let mut rb = ReadBuf::new(&mut buf);
        assert!(matches!(
            reader.as_mut().poll_read(&mut cx, &mut rb),
            Poll::Pending
        ));

        tail_pos.store(9, Ordering::Release);

        let mut buf = [0; 1];
        let mut rb = ReadBuf::new(&mut buf);
        assert!(matches!(
            reader.as_mut().poll_read(&mut cx, &mut rb),
            Poll::Ready(Err(_))
        ));
    }

    #[test]
    fn test_writer_wraparound_preserves_order() {
        let mut cx = Context::from_waker(noop_waker_ref());
        let channel = Arc::new(Channel::new(4));
        let mut writer = pin!(channel.new_strong_writer());
        let mut reader = pin!(channel.new_strong_reader());

        let mut buf = [0u8; 3];
        {
            let mut rb = ReadBuf::new(&mut buf);
            assert!(matches!(
                writer.as_mut().poll_write(&mut cx, &[1, 2, 3]),
                Poll::Ready(Ok(3))
            ));
            assert!(matches!(
                reader.as_mut().poll_read(&mut cx, &mut rb),
                Poll::Ready(Ok(()))
            ));
            assert_eq!(rb.filled().len(), 3);
        }
        assert_eq!(buf, [1, 2, 3]);

        let mut buf2 = [0u8; 2];
        let mut rb2 = ReadBuf::new(&mut buf2);
        assert!(matches!(
            writer.as_mut().poll_write(&mut cx, &[4, 5]),
            Poll::Ready(Ok(2))
        ));
        assert!(matches!(
            reader.as_mut().poll_read(&mut cx, &mut rb2),
            Poll::Ready(Ok(()))
        ));
        assert_eq!(rb2.filled().len(), 2);
        assert_eq!(buf2, [4, 5]);
    }

    #[test]
    fn test_terminate() {
        let mut cx = Context::from_waker(noop_waker_ref());
        let channel = Arc::new(Channel::new(4));
        let mut writer = channel.new_writer();
        let mut strong_reader = pin!(channel.new_strong_reader());
        let mut weak_reader = pin!(channel.new_weak_reader());
        let mut weak_reader2 = pin!(channel.new_weak_reader());
        let mut buf = [0; 4];
        let mut rb = ReadBuf::new(&mut buf);

        writer.terminate();
        let mut writer = pin!(writer);
        assert!(check_poll_aborted(
            writer.as_mut().poll_write(&mut cx, &[]).map_ok(|_| ())
        ));

        assert!(matches!(
            strong_reader.as_mut().poll_read(&mut cx, &mut rb),
            Poll::Pending
        ));
        assert!(matches!(
            weak_reader.as_mut().poll_read(&mut cx, &mut rb),
            Poll::Pending
        ));
        assert!(matches!(
            weak_reader2.as_mut().poll_read(&mut cx, &mut rb),
            Poll::Pending
        ));

        strong_reader.terminate();
        assert!(check_poll_aborted(
            strong_reader.poll_read(&mut cx, &mut rb)
        ));

        assert!(matches!(
            weak_reader.as_mut().poll_read(&mut cx, &mut rb),
            Poll::Pending
        ));
        assert!(matches!(
            weak_reader2.as_mut().poll_read(&mut cx, &mut rb),
            Poll::Pending
        ));

        weak_reader.terminate();
        assert!(check_poll_aborted(weak_reader.poll_read(&mut cx, &mut rb)));

        assert!(matches!(
            weak_reader2.as_mut().poll_read(&mut cx, &mut rb),
            Poll::Pending
        ));

        channel.terminate().unwrap();
        assert!(check_poll_aborted(weak_reader2.poll_read(&mut cx, &mut rb)));
    }

    #[test]
    fn strong_reader_terminate_aborts_read() {
        let mut cx = Context::from_waker(noop_waker_ref());
        let channel = Arc::new(Channel::new(16));
        let reader = channel.new_strong_reader();
        reader.terminate();
        let mut reader = pin!(reader);
        let mut buf = [0u8; 1];
        let mut rb = ReadBuf::new(&mut buf);
        assert!(check_poll_aborted(
            reader.as_mut().poll_read(&mut cx, &mut rb)
        ));
    }

    #[test]
    fn writer_terminate_aborts_write() {
        let mut cx = Context::from_waker(noop_waker_ref());
        let channel = Arc::new(Channel::new(16));
        let mut writer = channel.new_writer();
        writer.terminate();
        let mut writer = pin!(writer);
        assert!(check_poll_aborted(
            writer.as_mut().poll_write(&mut cx, &[]).map_ok(|_| ())
        ));
    }

    fn check_poll_aborted(poll: Poll<std::io::Result<()>>) -> bool {
        match poll {
            Poll::Ready(r) => r.is_err_and(|e| e.kind() == std::io::ErrorKind::ConnectionAborted),
            _ => false,
        }
    }

    #[test]
    fn test_drop_backpressure_writer_returns_zero_when_full() {
        let mut cx = Context::from_waker(noop_waker_ref());
        // Small buffer to hit full condition quickly
        let channel = Arc::new(Channel::with_parameters(4, Backpressure::Drop));
        let mut writer = pin!(channel.new_writer());
        // Add a strong reader pinned at start (0) so head stays at 0
        let _reader = pin!(channel.new_strong_reader());

        // First write fills the buffer
        assert!(matches!(
            writer.as_mut().poll_write(&mut cx, &[1, 2, 3, 4]),
            Poll::Ready(Ok(4))
        ));
        // Next write finds no space and, in Drop mode, returns Ok(0)
        assert!(matches!(
            writer.as_mut().poll_write(&mut cx, &[9]),
            Poll::Ready(Ok(0))
        ));
    }
}

#[cfg(all(test, feature = "loom"))]
mod loom_tests {
    use futures::future;
    use loom::future::block_on;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;

    #[test]
    fn strong_reader_and_writer_progress() {
        loom::model(|| {
            let channel = Channel::new(4);
            let mut reader = channel.new_strong_reader();
            let mut writer = channel.new_writer();

            block_on(async move {
                let mut buf = [0u8; 1];
                let read = reader.read_exact(&mut buf);
                let write = writer.write_all(&[42]);
                let (_w, r) = future::join(write, read).await;
                r.unwrap();
                assert_eq!(buf[0], 42);
            });
        });
    }
}
