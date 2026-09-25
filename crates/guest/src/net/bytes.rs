//! Shared two-ring byte-stream channel over shared-memory ring buffers.
//!
//! [`ByteStream`] is the byte-channel substrate shared by [`TcpStream`]
//! (via `net::tcp`) and the QUIC connector's per-stream channels: it attaches
//! to — or wraps — a two-ring multi-memory region (the
//! [`selium_memory::MultiMemoryHeader`] layout consumed by
//! [`selium_shm::byte_channel`]) and presents `tokio::io::AsyncRead +
//! AsyncWrite` by prepending/stripping [`FrameHeader`]s, so callers see one
//! continuous byte stream.
//!
//! TCP and QUIC byte channels deliberately share this one layout and this one
//! implementation. The variation is the peer model: TCP rings are driven by
//! the runtime's host proxy threads, so both halves are plain; QUIC channels
//! are peer-to-peer between two guests, so both halves are **blocking** — the
//! writer registers a writer count (so a peer's reader observes EOF when the
//! writing peer drops its writer) and the reader holds a reader slot and
//! wakes parked writers as it consumes (without which a full ring deadlocks:
//! only a reader can free capacity). A [`ByteStream`] can be [`split`] into
//! independent [`ByteStreamReader`]/[`ByteStreamWriter`] halves, so a relay
//! can finish one direction (dropping its writer) while the other continues.

use std::{
    pin::Pin,
    task::{Context, Poll},
};

use selium_abi::{RegionProt, SharedRegionDescriptor};
use selium_memory::{FrameHeader, MultiMemoryHeader, Region};
use selium_shm::{
    Channel, ChannelBackpressure, ChannelRegion,
    channels::{BlockingReader, BlockingWriter, Reader, Writer},
    ring_buf::RingBuf,
};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

use crate::{GuestError, Result};

/// The write half of a byte stream, either non-blocking (TCP) or blocking
/// (QUIC channels).
pub enum StreamWriter {
    /// Non-blocking writer; not tracked in `writer_count`.
    Plain(Writer),
    /// Blocking writer; increments `writer_count` so a peer reader detects
    /// close/EOF when this writer drops.
    Blocking(BlockingWriter),
}

/// The read half of a byte stream: decodes frames from the inbound ring and
/// strips headers, presenting a continuous byte stream.
///
/// Two reader kinds: a plain [`Reader`] (non-blocking, host-proxy-driven
/// rings, e.g. TCP) and a [`BlockingReader`] (peer-to-peer rings): the
/// blocking kind holds a reader slot so the peer's writer parks instead of
/// overwriting, and it wakes parked writers as it consumes — without that,
/// a peer-to-peer channel deadlocks the first time the writer fills the
/// ring (nothing ever frees or notifies capacity).
pub struct ByteStreamReader {
    reader: StreamReader,
    /// Buffered data from a partial frame read (header already stripped).
    read_buf: Vec<u8>,
    /// Offset into `read_buf` for the next copy.
    read_offset: usize,
    /// Keeps the parent shared region alive while the stream is in use.
    _region: Region,
}

/// The ring reader underlying a [`ByteStreamReader`].
enum StreamReader {
    /// Non-blocking reader; not tracked in the ring's reader positions.
    Plain(Reader),
    /// Blocking reader: holds a reader slot, wakes parked writers on
    /// consume, and surfaces EOF when the peer's writer count drops to
    /// zero.
    Blocking(BlockingReader),
}

/// The write half of a byte stream: encodes each write as one frame
/// (header + payload) on the outbound ring.
pub struct ByteStreamWriter {
    writer: StreamWriter,
    /// Keeps the parent shared region alive while the stream is in use.
    _region: Region,
}

/// A byte stream backed by a two-ring shared-memory region.
///
/// Mirrors `TcpStream`'s byte-channel semantics: each `poll_write` encodes one
/// frame (header + payload) on the outbound ring; each `poll_read` decodes
/// frames from the inbound ring and strips headers.
pub struct ByteStream {
    reader: ByteStreamReader,
    writer: ByteStreamWriter,
}

impl AsyncWrite for StreamWriter {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        match self.get_mut() {
            StreamWriter::Plain(writer) => Pin::new(writer).poll_write(cx, buf),
            StreamWriter::Blocking(writer) => Pin::new(writer).poll_write(cx, buf),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            StreamWriter::Plain(writer) => Pin::new(writer).poll_flush(cx),
            StreamWriter::Blocking(writer) => Pin::new(writer).poll_flush(cx),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            StreamWriter::Plain(writer) => Pin::new(writer).poll_shutdown(cx),
            StreamWriter::Blocking(writer) => Pin::new(writer).poll_shutdown(cx),
        }
    }
}

impl ByteStreamReader {
    /// Wraps an attached ring reader as the read half of a byte stream.
    pub fn new(reader: Reader, region: Region) -> Self {
        Self {
            reader: StreamReader::Plain(reader),
            read_buf: Vec::new(),
            read_offset: 0,
            _region: region,
        }
    }

    /// Wraps an attached blocking ring reader as the read half of a
    /// peer-to-peer byte stream (see [`StreamReader::Blocking`]).
    pub fn blocking(reader: BlockingReader, region: Region) -> Self {
        Self {
            reader: StreamReader::Blocking(reader),
            read_buf: Vec::new(),
            read_offset: 0,
            _region: region,
        }
    }
}

impl AsyncRead for ByteStreamReader {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let this = self.get_mut();

        // Drain any previously buffered payload bytes (header already stripped).
        if this.read_offset < this.read_buf.len() {
            let remaining = this.read_buf.get(this.read_offset..).unwrap_or(&[]);
            let to_copy = remaining.len().min(buf.remaining());
            buf.put_slice(remaining.get(..to_copy).unwrap_or(remaining));
            this.read_offset += to_copy;
            if this.read_offset >= this.read_buf.len() {
                this.read_buf.clear();
                this.read_offset = 0;
            }
            return Poll::Ready(Ok(()));
        }

        // Read a full frame into a temporary buffer, then strip the header.
        let mut frame_buf = vec![0u8; 65536];
        let mut inner_buf = ReadBuf::new(&mut frame_buf);

        match Pin::new(&mut this.reader).poll_read_frame(cx, &mut inner_buf) {
            Poll::Ready(Ok(())) => {
                let filled = inner_buf.filled().len();
                if filled == 0 {
                    return Poll::Ready(Ok(())); // EOF
                }
                let header_size = FrameHeader::ENCODED_SIZE;
                if filled <= header_size {
                    return Poll::Ready(Ok(()));
                }
                let payload = frame_buf.get(header_size..filled).unwrap_or(&[]).to_vec();

                // Store stripped payload in read_buf for potential partial copy.
                this.read_buf = payload;
                this.read_offset = 0;

                let to_copy = this.read_buf.len().min(buf.remaining());
                buf.put_slice(this.read_buf.get(..to_copy).unwrap_or(&[]));
                this.read_offset = to_copy;
                if this.read_offset >= this.read_buf.len() {
                    this.read_buf.clear();
                    this.read_offset = 0;
                }
                Poll::Ready(Ok(()))
            }
            Poll::Ready(Err(e)) => Poll::Ready(Err(e)),
            Poll::Pending => Poll::Pending,
        }
    }
}

// SAFETY: see the `Send` impl on `ByteStream`.
unsafe impl Send for ByteStreamReader {}

impl StreamReader {
    fn poll_read_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        match &mut *self {
            StreamReader::Plain(reader) => Pin::new(reader).poll_read(cx, buf),
            StreamReader::Blocking(reader) => Pin::new(reader).poll_read(cx, buf),
        }
    }
}

impl ByteStreamWriter {
    /// Wraps a non-blocking ring writer as the write half of a byte stream.
    pub fn plain(writer: Writer, region: Region) -> Self {
        Self {
            writer: StreamWriter::Plain(writer),
            _region: region,
        }
    }

    /// Wraps a blocking ring writer as the write half of a byte stream.
    pub fn blocking(writer: BlockingWriter, region: Region) -> Self {
        Self {
            writer: StreamWriter::Blocking(writer),
            _region: region,
        }
    }
}

impl AsyncWrite for ByteStreamWriter {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let this = self.get_mut();
        let payload_len = buf.len() as u32;
        let header = FrameHeader {
            len: payload_len,
            tag: 0,
            flags: 0,
        };
        let header_bytes = header.encode();

        // Build the framed buffer: [FrameHeader 12 bytes][user payload].
        let mut write_buf = Vec::with_capacity(header_bytes.len() + buf.len());
        write_buf.extend_from_slice(&header_bytes);
        write_buf.extend_from_slice(buf);

        match Pin::new(&mut this.writer).poll_write(cx, &write_buf) {
            Poll::Ready(Ok(n)) => {
                // Report payload bytes written (not header+payload).
                let payload_written = n.saturating_sub(header_bytes.len());
                Poll::Ready(Ok(payload_written))
            }
            Poll::Ready(Err(e)) => Poll::Ready(Err(e)),
            Poll::Pending => Poll::Pending,
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().writer).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().writer).poll_shutdown(cx)
    }
}

// SAFETY: see the `Send` impl on `ByteStream`.
unsafe impl Send for ByteStreamWriter {}

impl ByteStream {
    /// Attaches to a two-ring stream region by `shared_id` as the "primary"
    /// peer: ring `0` is the inbound ring (read side) and ring `1` is the
    /// outbound ring (write side), matching `TcpStream`'s layout. Uses a
    /// non-blocking writer.
    pub fn attach(shared_id: u64) -> Result<Self> {
        Self::attach_inner(shared_id, false)
    }

    /// Like [`attach`](Self::attach), but uses a blocking writer so the
    /// attach-side peer's close is observable by the writing-side peer.
    pub fn attach_blocking(shared_id: u64) -> Result<Self> {
        Self::attach_inner(shared_id, true)
    }

    /// Attaches to a stream region described by a host-provided descriptor.
    pub fn attach_descriptor(descriptor: SharedRegionDescriptor) -> Result<Self> {
        Self::attach(descriptor.shared_id)
    }

    /// Builds a `ByteStream` from already-attached ring channels and the
    /// parent region that keeps them alive.
    ///
    /// Callers that allocate their own region (the QUIC connector) name the
    /// read/write halves explicitly, since each peer's "inbound" is the other
    /// peer's "outbound". Pass `blocking = true` for peer-to-peer channels
    /// so writer-count close semantics and reader-slot backpressure work in
    /// both directions.
    pub fn from_ring_channels(
        reader_channel: &Channel,
        writer_channel: &Channel,
        region: Region,
        blocking: bool,
    ) -> Result<Self> {
        let (reader, writer) =
            Self::halves_from_channels(reader_channel, writer_channel, region.clone(), blocking)?;
        Ok(Self { reader, writer })
    }

    /// Builds independent read/write halves from ring channels.
    ///
    /// `blocking` selects peer-to-peer semantics for **both** halves: a
    /// blocking reader (slot + wake-on-consume) and a blocking writer
    /// (writer count + EOF-to-peer). With `blocking = false` both halves
    /// are plain (host-proxy-driven rings, e.g. TCP, where the runtime's
    /// proxy threads own the peer side).
    pub fn halves_from_channels(
        reader_channel: &Channel,
        writer_channel: &Channel,
        region: Region,
        blocking: bool,
    ) -> Result<(ByteStreamReader, ByteStreamWriter)> {
        let reader = if blocking {
            ByteStreamReader::blocking(
                reader_channel
                    .blocking_reader_from(0)
                    .map_err(|e| GuestError::Host(format!("create blocking reader failed: {e}")))?,
                region.clone(),
            )
        } else {
            ByteStreamReader::new(reader_channel.reader(), region.clone())
        };
        let writer = if blocking {
            ByteStreamWriter::blocking(
                writer_channel
                    .blocking_writer()
                    .map_err(|e| GuestError::Host(format!("create blocking writer failed: {e}")))?,
                region,
            )
        } else {
            ByteStreamWriter::plain(
                writer_channel
                    .writer()
                    .map_err(|e| GuestError::Host(format!("create writer failed: {e}")))?,
                region,
            )
        };
        Ok((reader, writer))
    }

    /// Splits into independent read and write halves with their own lifetimes.
    pub fn split(self) -> (ByteStreamReader, ByteStreamWriter) {
        (self.reader, self.writer)
    }

    /// Returns the read half.
    pub fn reader(&mut self) -> &mut ByteStreamReader {
        &mut self.reader
    }

    /// Returns the write half.
    pub fn writer(&mut self) -> &mut ByteStreamWriter {
        &mut self.writer
    }

    fn attach_inner(shared_id: u64, blocking: bool) -> Result<Self> {
        let region_provider = selium_memory::region_provider()
            .map_err(|e| GuestError::Host(format!("region provider unavailable: {e}")))?;

        let region = region_provider
            .attach(shared_id, None, RegionProt::ReadWrite)
            .map_err(|e| GuestError::Host(format!("attach region failed: {e}")))?;

        let mapping = region.mapping();
        let header = MultiMemoryHeader::parse(mapping.backend(), 0)
            .map_err(|e| GuestError::Host(format!("parse header failed: {e}")))?;

        let inbound = header
            .entry(0)
            .map_err(|e| GuestError::Host(format!("inbound entry missing: {e}")))?;
        let outbound = header
            .entry(1)
            .map_err(|e| GuestError::Host(format!("outbound entry missing: {e}")))?;

        let inbound_mapping = mapping
            .sub_region(inbound.offset, inbound.length)
            .map_err(|e| GuestError::Host(format!("inbound sub-region failed: {e}")))?;
        let outbound_mapping = mapping
            .sub_region(outbound.offset, outbound.length)
            .map_err(|e| GuestError::Host(format!("outbound sub-region failed: {e}")))?;

        // Ring data capacity = region length minus header overhead.
        let ring_cap = inbound
            .length
            .saturating_sub(selium_shm::layout::DATA_OFFSET);

        let inbound_region =
            ChannelRegion::from_mapping_with_id(inbound_mapping, ring_cap, shared_id);
        let outbound_region =
            ChannelRegion::from_mapping_with_id(outbound_mapping, ring_cap, shared_id);

        let inbound_ring = RingBuf::wrap_region(inbound_region)
            .map_err(|e| GuestError::Host(format!("wrap inbound ring failed: {e}")))?;
        let outbound_ring = RingBuf::wrap_region(outbound_region)
            .map_err(|e| GuestError::Host(format!("wrap outbound ring failed: {e}")))?;

        let inbound_channel = Channel::from_ring(inbound_ring, ChannelBackpressure::Park);
        let outbound_channel = Channel::from_ring(outbound_ring, ChannelBackpressure::Park);

        Self::from_ring_channels(&inbound_channel, &outbound_channel, region, blocking)
    }
}

impl AsyncRead for ByteStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().reader).poll_read(cx, buf)
    }
}

impl AsyncWrite for ByteStream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.get_mut().writer).poll_write(cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().writer).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().writer).poll_shutdown(cx)
    }
}

// SAFETY: Reader, Writer, BlockingReader, and BlockingWriter are safe to
// send across threads (they encapsulate shared memory regions backed by
// process-level mappings).
unsafe impl Send for ByteStream {}
