use std::{
    pin::Pin,
    sync::atomic::{Ordering, fence},
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
    pub fn upgrade(self) -> Result<BlockingWriter> {
        self.region.increment_writer_count()?;
        let writer_slot = match self.region.allocate_writer_slot(0) {
            Ok(slot) => slot,
            Err(error) => {
                drop(self.region.decrement_writer_count());
                return Err(error);
            }
        };
        Ok(BlockingWriter {
            region: self.region,
            writer_id: self.writer_id,
            writer_slot,
            backpressure: self.backpressure,
            closed: false,
        })
    }
}

impl AsyncWrite for Writer {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
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
        Poll::Ready(Ok(()))
    }
}

impl BlockingWriter {
    /// Creates a new blocking writer. Increments writer_count, allocates a writer_id,
    /// and allocates a writer slot for position tracking.
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
        Ok(Self {
            region,
            writer_id,
            writer_slot,
            backpressure,
            closed: false,
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
        self.release_writer_registration();
        Writer {
            region: self.region.clone(),
            writer_id: self.writer_id,
            backpressure: self.backpressure,
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
    fn release_writer_registration(&self) {
        drop(self.region.decrement_writer_count());
        drop(self.region.release_writer_slot(self.writer_slot));
        drop(self.region.bump_generation());
    }
}

impl AsyncWrite for BlockingWriter {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        if buf.is_empty() {
            return Poll::Ready(Ok(0));
        }
        let len = buf.len() as u64;
        if len > self.region.capacity() {
            return Poll::Ready(Err(std::io::Error::other(Error::BufferFull)));
        }

        // Blocking writers check both reader and writer positions.
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

        // Update writer slot to this position so other writers cannot
        // overwrite our data until we write again.
        if let Err(e) = self.region.update_writer_slot(self.writer_slot, pos) {
            return Poll::Ready(Err(std::io::Error::other(e)));
        }

        if let Err(e) = write_frame_bytes(&self.region, pos, buf) {
            return Poll::Ready(Err(std::io::Error::other(e)));
        }

        Poll::Ready(Ok(buf.len()))
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

impl Drop for BlockingWriter {
    fn drop(&mut self) {
        if self.closed {
            return;
        }
        self.closed = true;
        self.release_writer_registration();
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
    let mask = region.capacity() - 1;

    let header_size = FrameHeader::ENCODED_SIZE as u64;
    let payload = buf.get(FrameHeader::ENCODED_SIZE..).unwrap_or_default();

    // The pre-encoded header carried by `buf`; decode validates its checksum.
    let frame_header =
        FrameHeader::decode(buf.get(..FrameHeader::ENCODED_SIZE).unwrap_or_default())
            .map_err(|e| Error::InvalidFrame(e.to_string()))?;

    // Step 1: Placeholder header (checksum-valid, not READY).
    let mut not_ready = frame_header;
    not_ready.flags &= !FrameHeader::FLAG_READY;
    write_raw(region, pos, &not_ready.encode(), mask)?;

    // Step 2: Write payload at pos + ENCODED_SIZE.
    let payload_pos = pos
        .checked_add(header_size)
        .ok_or_else(|| Error::InvalidFrame("payload position overflow".to_string()))?;
    write_raw(region, payload_pos, payload, mask)?;

    // Step 3: Release fence ensures payload is visible before the header.
    fence(Ordering::Release);

    // Step 4: Write header at pos with FLAG_READY set. Readers gate on
    // `is_ready()`; committing without it makes the frame permanently
    // invisible to them (matches the canonical layout::write_frame).
    let mut ready_header = frame_header;
    ready_header.flags |= FrameHeader::FLAG_READY;
    write_raw(region, pos, &ready_header.encode(), mask)?;

    // Step 5: Bump generation counter and notify waiters.
    region.bump_generation()?;

    Ok(())
}

fn write_raw(region: &ChannelRegion, pos: u64, data: &[u8], mask: u64) -> Result<()> {
    if data.len() as u64 > region.capacity() {
        return Err(Error::BufferFull);
    }
    let raw_start = (pos & mask) as usize;
    let tail = data.len().min(region.capacity() as usize - raw_start);
    let head = data.len() - tail;

    if tail > 0 {
        let offset = region.data_offset() + raw_start as u64;
        region
            .mapping()
            .write(offset, data.get(..tail).unwrap_or_default())?;
    }
    if head > 0 {
        let offset = region.data_offset();
        region
            .mapping()
            .write(offset, data.get(tail..).unwrap_or_default())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use selium_memory::FrameHeader;

    use super::*;
    use crate::ChannelRegion;
    use crate::channels::reader::read_raw;

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
        let header_bytes =
            read_raw(&region, pos, FrameHeader::ENCODED_SIZE as u64, mask).expect("read header");
        let decoded = FrameHeader::decode(&header_bytes).expect("decode");
        assert!(decoded.is_ready());
        assert_eq!(decoded.len, 5);
        assert_eq!(decoded.tag, 1);

        // Read back the payload.
        let payload_pos = pos + FrameHeader::ENCODED_SIZE as u64;
        let payload = read_raw(&region, payload_pos, 5, mask).expect("read payload");
        assert_eq!(payload, b"hello");
    }
}
