use std::{
    pin::Pin,
    task::{Context, Poll},
};

use selium_memory::FrameHeader;
use selium_wire::error::{Error, Result};
use tokio::io::AsyncWrite;

use crate::{channels::ChannelBackpressure, region::ChannelRegion};

/// Non-blocking byte-stream writer not tracked in channel metadata; may be starved
/// by other writers. Implements [`AsyncWrite`].
///
/// On Park channels, writes block when a blocking reader or blocking writer is slow.
/// On Drop channels, writes silently drop data when a blocking reader or blocking
/// writer is slow.
pub struct Writer {
    region: ChannelRegion,
    writer_id: u32,
    backpressure: ChannelBackpressure,
    closed: bool,
}

/// Blocking byte-stream writer tracked in the channel metadata, preventing
/// buffer overwrite until the slowest blocking reader has consumed the data.
///
/// Implements [`AsyncWrite`] for raw byte-stream production. Each
/// `poll_write` call takes a buffer of raw frame bytes (`[header:12][payload:N]`)
/// and writes them to the ring buffer using a two-phase protocol (payload
/// first, release fence, header) to guarantee reader consistency.
///
/// The blocking writer registers a writer slot in shared memory. Other writers
/// cannot advance past the blocking writer's position, preventing a single
/// busy writer from starving the blocking writer. On Park channels this causes
/// backpressure; on Drop channels other writers silently drop data.
///
/// Use [`FramedWrite`](crate::io::framed::FramedWrite) for frame-level
/// operations with automatic header encoding.
pub struct BlockingWriter {
    region: ChannelRegion,
    writer_id: u32,
    writer_slot: u32,
    backpressure: ChannelBackpressure,
    closed: bool,
    writer_count_released: bool,
    writer_slot_released: bool,
}

impl Writer {
    pub(crate) fn new(
        region: ChannelRegion,
        writer_id: u32,
        backpressure: ChannelBackpressure,
    ) -> Self {
        Self {
            region,
            writer_id,
            backpressure,
            closed: false,
        }
    }

    /// Returns the writer id stored in emitted frames.
    pub fn writer_id(&self) -> u32 {
        self.writer_id
    }

    /// Returns the backpressure strategy for this writer's channel.
    pub fn backpressure(&self) -> ChannelBackpressure {
        self.backpressure
    }

    /// Allocates a globally unique mutation id for this writer's channel.
    pub fn allocate_mutation_id(&self) -> Result<u64> {
        self.region.allocate_mutation_id()
    }

    /// Returns a reference to the underlying channel region.
    pub fn region(&self) -> &ChannelRegion {
        &self.region
    }

    /// Upgrade this non-blocking writer to a blocking writer. Increments writer_count
    /// and allocates a writer slot for position tracking.
    ///
    /// Successful registration publishes the attachment through the ring's
    /// generation counter so readers parked on an empty ring wake and observe
    /// the new writer; a partial failure rolls the count back without
    /// publishing.
    pub fn upgrade(self) -> Result<BlockingWriter> {
        self.region.increment_writer_count()?;
        let writer_slot = match self.region.allocate_writer_slot(0) {
            Ok(slot) => slot,
            Err(error) => {
                drop(self.region.decrement_writer_count());
                return Err(error);
            }
        };
        if let Err(error) = self.region.bump_generation() {
            drop(self.region.release_writer_slot(writer_slot));
            drop(self.region.decrement_writer_count());
            return Err(error);
        }
        Ok(BlockingWriter {
            region: self.region,
            writer_id: self.writer_id,
            writer_slot,
            backpressure: self.backpressure,
            closed: false,
            writer_count_released: false,
            writer_slot_released: false,
        })
    }
}

impl AsyncWrite for Writer {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        if self.closed {
            return Poll::Ready(Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "writer has been shut down",
            )));
        }
        if buf.is_empty() {
            return Poll::Ready(Ok(0));
        }
        let len = buf.len() as u64;
        if len > self.region.capacity() {
            return Poll::Ready(Err(std::io::Error::other(Error::BufferFull)));
        }

        // All writers check both reader and writer positions.
        // On Park channels, BufferFull causes backpressure (Pending).
        // On Drop channels, BufferFull causes silent drop (Ok without writing).
        let pos = match self.region.reserve_tail(len, true, true) {
            Ok(p) => p,
            Err(Error::BufferFull) => {
                if self.backpressure == ChannelBackpressure::Drop {
                    return Poll::Ready(Ok(buf.len()));
                }
                // Park: register for generation wake.
                let cur_gen = self.region.load_generation().unwrap_or(0);
                if !selium_memory::register_generation_wait(
                    self.region.region_id(),
                    cur_gen,
                    cx.waker(),
                ) {
                    cx.waker().wake_by_ref();
                }
                return Poll::Pending;
            }
            Err(e) => return Poll::Ready(Err(std::io::Error::other(e))),
        };

        if let Err(e) = write_frame_bytes(&self.region, pos, buf) {
            return Poll::Ready(Err(std::io::Error::other(e)));
        }

        Poll::Ready(Ok(buf.len()))
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        let this = self.get_mut();
        if !this.closed {
            // A plain writer holds no writer count/slot, but readers parked on
            // the generation word must still observe the close. Do not mark
            // the writer closed until that wake has actually succeeded.
            if let Err(error) = this.region.bump_generation() {
                return Poll::Ready(Err(std::io::Error::other(error)));
            }
            this.closed = true;
        }
        Poll::Ready(Ok(()))
    }
}

impl BlockingWriter {
    /// Creates a new blocking writer. Increments writer_count, allocates a writer_id,
    /// and allocates a writer slot for position tracking.
    ///
    /// On success the attachment is published through the ring's generation
    /// counter (see [`BlockingReader`](super::BlockingReader)), which wakes
    /// readers parked on an empty ring. A partial failure releases the count
    /// and slot it took without publishing an attachment.
    pub fn new(region: ChannelRegion, backpressure: ChannelBackpressure) -> Result<Self> {
        region.increment_writer_count()?;
        let writer_id = match region.allocate_writer_id() {
            Ok(id) => id,
            Err(error) => {
                drop(region.decrement_writer_count());
                return Err(error);
            }
        };
        let writer_slot = match region.allocate_writer_slot(0) {
            Ok(slot) => slot,
            Err(error) => {
                drop(region.decrement_writer_count());
                return Err(error);
            }
        };
        if let Err(error) = region.bump_generation() {
            drop(region.release_writer_slot(writer_slot));
            drop(region.decrement_writer_count());
            return Err(error);
        }
        Ok(Self {
            region,
            writer_id,
            writer_slot,
            backpressure,
            closed: false,
            writer_count_released: false,
            writer_slot_released: false,
        })
    }

    /// Returns a reference to the underlying channel region.
    pub fn region(&self) -> &ChannelRegion {
        &self.region
    }

    /// Returns the writer id stored in emitted frames.
    pub fn writer_id(&self) -> u32 {
        self.writer_id
    }

    /// Returns the backpressure strategy for this writer's channel.
    pub fn backpressure(&self) -> ChannelBackpressure {
        self.backpressure
    }

    /// Allocates a globally unique mutation id for this writer's channel.
    pub fn allocate_mutation_id(&self) -> Result<u64> {
        self.region.allocate_mutation_id()
    }

    /// Downgrade this blocking writer to a non-blocking writer.
    /// Decrements writer_count and releases the writer slot to compensate
    /// for the lost Drop decrement.
    pub fn downgrade(mut self) -> Writer {
        self.closed = true;
        drop(self.release_writer_registration());
        Writer {
            region: self.region.clone(),
            writer_id: self.writer_id,
            backpressure: self.backpressure,
            closed: false,
        }
    }

    /// Withdraws this writer from the ring's writer count and slot table,
    /// then bumps the generation so every reader parked on the ring is
    /// woken to observe the new writer count.
    ///
    /// The bump is what makes close observable: a reader that found the
    /// ring empty parks with a generation wait, and a bare writer-count
    /// decrement advances nothing and wakes nobody — the reader would sleep
    /// forever with `writer_count == 0` (EOF) sitting unread in shared
    /// memory. Treating close as an EOF generation bump mirrors the host
    /// poller's socket-EOF path and reaches every waiter kind: same-guest
    /// wakers, cross-guest `WaitRegister` tasks (via the `GenerationAdvance`
    /// hostcall), and host drainers parked on the generation word.
    fn release_writer_registration(&mut self) -> Result<()> {
        if !self.writer_count_released {
            self.region.decrement_writer_count()?;
            self.writer_count_released = true;
        }
        if !self.writer_slot_released {
            self.region.release_writer_slot(self.writer_slot)?;
            self.writer_slot_released = true;
        }
        self.region.bump_generation()?;
        Ok(())
    }
}

impl BlockingWriter {
    /// Returns whether one encoded frame of `payload_len` bytes fits in the
    /// ring without overwriting an active reader.
    ///
    /// Host proxies use this before consuming a socket datagram: peeking first
    /// lets a full ring leave the datagram in the kernel receive buffer instead
    /// of reading and dropping it.
    pub fn has_capacity_for_frame(&self, payload_len: u64) -> Result<bool> {
        let frame_len = FrameHeader::ENCODED_SIZE as u64 + payload_len;
        if frame_len > self.region.capacity() {
            return Ok(false);
        }
        let next_tail = self.region.read_next_tail()?;
        let Some(min_reader) = self.region.minimum_reader_position()? else {
            return Ok(true);
        };
        let used = next_tail.saturating_sub(min_reader);
        Ok(self.region.capacity().saturating_sub(used) >= frame_len)
    }
}

impl AsyncWrite for BlockingWriter {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        if self.closed {
            return Poll::Ready(Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "writer has been shut down",
            )));
        }
        if buf.is_empty() {
            return Poll::Ready(Ok(0));
        }
        let len = buf.len() as u64;
        if len > self.region.capacity() {
            return Poll::Ready(Err(std::io::Error::other(Error::BufferFull)));
        }

        // Blocking writers check both reader and writer positions. Their own
        // slot is excluded: it anchors *other* writers, never themselves.
        // On Park channels, BufferFull causes backpressure (Pending).
        // On Drop channels, BufferFull causes silent drop (Ok without writing).
        let pos = match self
            .region
            .reserve_tail_for_writer(len, true, self.writer_slot)
        {
            Ok(p) => p,
            Err(Error::BufferFull) => {
                if self.backpressure == ChannelBackpressure::Drop {
                    return Poll::Ready(Ok(buf.len()));
                }
                // Park: register for generation wake.
                let cur_gen = self.region.load_generation().unwrap_or(0);
                if !selium_memory::register_generation_wait(
                    self.region.region_id(),
                    cur_gen,
                    cx.waker(),
                ) {
                    cx.waker().wake_by_ref();
                }
                return Poll::Pending;
            }
            Err(e) => return Poll::Ready(Err(std::io::Error::other(e))),
        };

        // Anchor our slot at the start of the reserved frame while it is in
        // flight: other writers must not wrap past it until the write
        // completes.
        if let Err(e) = self.region.update_writer_slot(self.writer_slot, pos) {
            return Poll::Ready(Err(std::io::Error::other(e)));
        }

        if let Err(e) = write_frame_bytes(&self.region, pos, buf) {
            return Poll::Ready(Err(std::io::Error::other(e)));
        }

        // Advance the anchor past the committed frame. The reader slot (not
        // this one) protects committed-but-unread data, so leaving the anchor
        // at the frame start would shrink the window the writer has for its
        // next frame by this frame's size.
        if let Err(e) = self.region.update_writer_slot(self.writer_slot, pos + len) {
            return Poll::Ready(Err(std::io::Error::other(e)));
        }

        Poll::Ready(Ok(buf.len()))
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        let this = self.get_mut();
        if !this.closed {
            // Release writer registration and bump the generation: readers
            // parked on an empty ring wake, drain committed bytes, and observe
            // EOF (writer_count == 0). Each completed release step is recorded
            // so a failed shutdown can be retried without a double decrement.
            if let Err(error) = this.release_writer_registration() {
                return Poll::Ready(Err(std::io::Error::other(error)));
            }
            this.closed = true;
        }
        Poll::Ready(Ok(()))
    }
}

impl Drop for BlockingWriter {
    fn drop(&mut self) {
        if self.closed {
            return;
        }
        self.closed = true;
        drop(self.release_writer_registration());
    }
}

/// Two-phase write of an already-encoded frame (header + payload) to the ring buffer.
///
/// Writes a placeholder header (checksum-valid, not READY) at `pos` first, so
/// a strict checksum-verifying reader never mistakes an in-flight reservation's
/// stale bytes for a torn header. Then writes the payload, then a release
/// fence, then the READY header. This ensures readers never observe a READY
/// header before the completed payload. Finally bumps the generation counter
/// to notify waiters.
fn write_frame_bytes(region: &ChannelRegion, pos: u64, buf: &[u8]) -> Result<()> {
    region.write_encoded_frame(pos, buf)
}

#[cfg(test)]
mod tests {
    use selium_memory::FrameHeader;
    use tokio::io::AsyncRead as _;

    use super::*;
    use crate::ChannelRegion;

    #[test]
    fn two_phase_write_produces_ready_frame() {
        let region =
            ChannelRegion::create(64, selium_abi::ResourceKind::SharedMemory).expect("create");
        region.initialise().expect("init");

        // Encode a frame manually: header + payload.
        let header = FrameHeader {
            len: 5,
            tag: 1,
            flags: FrameHeader::FLAG_READY,
        };
        let mut buf = Vec::new();
        buf.extend_from_slice(&header.encode());
        buf.extend_from_slice(b"hello");

        let pos = region
            .reserve_tail(buf.len() as u64, true, false)
            .expect("reserve");
        write_frame_bytes(&region, pos, &buf).expect("write");

        // Read back the header and verify READY flag.
        let mask = region.capacity() - 1;
        let header_bytes = crate::layout::read_at(
            region.mapping().backend(),
            pos,
            FrameHeader::ENCODED_SIZE as u64,
            mask,
            region.capacity(),
        )
        .expect("read header");
        let decoded = FrameHeader::decode(&header_bytes).expect("decode");
        assert!(decoded.is_ready());
        assert_eq!(decoded.len, 5);
        assert_eq!(decoded.tag, 1);

        // Read back the payload.
        let payload_pos = pos + FrameHeader::ENCODED_SIZE as u64;
        let payload = crate::layout::read_at(
            region.mapping().backend(),
            payload_pos,
            5,
            mask,
            region.capacity(),
        )
        .expect("read payload");
        assert_eq!(payload, b"hello");
    }

    // scenario: selium-shm/reader-writer-wait-semantics/disconnect-is-observable
    // scenario: selium-guest/strong-writer-backpressure-via-asyncwrite/writer-sends-bytes
    // scenario: guest-networking/async-shutdown-closes-the-writer/shutdown-wakes-peer-with-eof
    // scenario: guest-networking/async-shutdown-closes-the-writer/write-after-shutdown
    #[tokio::test]
    async fn shutdown_releases_registration_signals_eof_and_rejects_writes() {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

        let channel =
            crate::Channel::create(4096, crate::ChannelBackpressure::Park).expect("create channel");
        let mut writer = channel.blocking_writer().expect("writer");
        let mut reader = channel.blocking_reader_from(0).expect("reader");

        // Write one committed frame, then shut the writer down.
        let header = FrameHeader {
            len: 5,
            tag: 0,
            flags: 0,
        };
        let mut encoded = header.encode().to_vec();
        encoded.extend_from_slice(b"hello");
        writer.write_all(&encoded).await.expect("write");
        writer.shutdown().await.expect("shutdown");

        assert_eq!(
            writer.region().load_writer_count().expect("writer count"),
            0,
            "shutdown releases the writer registration"
        );
        assert!(
            writer.write_all(&encoded).await.is_err(),
            "writes after shutdown must be rejected"
        );

        let mut received = vec![0u8; FrameHeader::ENCODED_SIZE + 5];
        reader.read_exact(&mut received).await.expect("read frame");
        assert_eq!(&received[FrameHeader::ENCODED_SIZE..], b"hello");

        let mut eof = [0u8; 8];
        assert_eq!(
            reader.read(&mut eof).await.expect("eof read"),
            0,
            "the peer observes EOF after the writer shuts down"
        );
    }

    #[test]
    fn shutdown_propagates_each_release_failure_and_can_be_retried() {
        for stage in [
            crate::region::RegionFailure::DecrementWriter,
            crate::region::RegionFailure::ReleaseWriterSlot,
            crate::region::RegionFailure::Generation,
            crate::region::RegionFailure::Notify,
        ] {
            let channel =
                crate::Channel::create(4096, crate::ChannelBackpressure::Park).expect("channel");
            let mut writer = channel.blocking_writer().expect("writer");
            crate::region::REGION_FAILURE.with(|failure| failure.set(Some(stage)));
            let waker = std::task::Waker::noop();
            let mut cx = Context::from_waker(waker);
            assert!(
                matches!(
                    Pin::new(&mut writer).poll_shutdown(&mut cx),
                    Poll::Ready(Err(_))
                ),
                "shutdown must surface {stage:?}"
            );
            assert!(
                !writer.closed,
                "failed shutdown must not mark the writer closed"
            );
            assert!(matches!(
                Pin::new(&mut writer).poll_shutdown(&mut cx),
                Poll::Ready(Ok(()))
            ));
            assert!(writer.closed);
            assert_eq!(writer.region().load_writer_count().expect("count"), 0);
            assert_eq!(
                writer
                    .region()
                    .load_writer_slot(writer.writer_slot)
                    .expect("slot"),
                0
            );
        }
    }

    #[test]
    fn plain_writer_shutdown_also_propagates_wake_failure() {
        let channel =
            crate::Channel::create(4096, crate::ChannelBackpressure::Park).expect("channel");
        let mut writer = channel.writer().expect("writer");
        crate::region::REGION_FAILURE
            .with(|failure| failure.set(Some(crate::region::RegionFailure::Generation)));
        let waker = std::task::Waker::noop();
        let mut cx = Context::from_waker(waker);
        assert!(matches!(
            Pin::new(&mut writer).poll_shutdown(&mut cx),
            Poll::Ready(Err(_))
        ));
        assert!(!writer.closed);
        assert!(matches!(
            Pin::new(&mut writer).poll_shutdown(&mut cx),
            Poll::Ready(Ok(()))
        ));
    }

    struct WakeFlag(std::sync::Arc<std::sync::atomic::AtomicBool>);

    impl std::task::Wake for WakeFlag {
        fn wake(self: std::sync::Arc<Self>) {
            self.0.store(true, std::sync::atomic::Ordering::Release);
        }

        fn wake_by_ref(self: &std::sync::Arc<Self>) {
            self.0.store(true, std::sync::atomic::Ordering::Release);
        }
    }

    #[test]
    fn shutdown_wakes_a_peer_that_was_already_parked() {
        let channel =
            crate::Channel::create(4096, crate::ChannelBackpressure::Park).expect("channel");
        let mut writer = channel.blocking_writer().expect("writer");
        let mut reader = channel.blocking_reader_from(0).expect("reader");
        let woken = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let waker = std::task::Waker::from(std::sync::Arc::new(WakeFlag(woken.clone())));
        let mut cx = Context::from_waker(&waker);
        let mut bytes = [0u8; 8];
        let mut buffer = tokio::io::ReadBuf::new(&mut bytes);
        assert!(
            Pin::new(&mut reader)
                .poll_read(&mut cx, &mut buffer)
                .is_pending()
        );
        assert!(matches!(
            Pin::new(&mut writer).poll_shutdown(&mut cx),
            Poll::Ready(Ok(()))
        ));
        assert!(woken.load(std::sync::atomic::Ordering::Acquire));
        assert!(matches!(
            Pin::new(&mut reader).poll_read(&mut cx, &mut buffer),
            Poll::Ready(Ok(()))
        ));
        assert!(buffer.filled().is_empty());
    }
}
