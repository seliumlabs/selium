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
use selium_memory::{FrameHeader, Region};
use selium_shm::{
    Channel,
    channels::{BlockingReader, BlockingWriter, Reader, Writer},
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

/// Peer lifecycle for a peer-to-peer byte-stream reader.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeerMode {
    /// The peer's existence is already established (a stream handoff
    /// recipient): an empty ring with no writers is EOF.
    Attached,
    /// Wait for the first peer before treating an empty, writer-less ring as
    /// EOF (the allocator side of a per-stream channel, whose serving peer
    /// attaches after the relay starts).
    AwaitingFirstPeer,
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
    /// Incremental frame state: header bytes accumulate until complete, then
    /// payload bytes are copied to the caller without buffering the whole
    /// frame.
    frame: FrameState,
    /// Keeps the parent shared region alive while the stream is in use.
    _region: Region,
}

/// How far through the current inbound frame the reader is.
enum FrameState {
    /// Accumulating the fixed 16-byte header.
    Header {
        bytes: [u8; FrameHeader::ENCODED_SIZE],
        filled: usize,
    },
    /// Copying `remaining` payload bytes from the ring.
    Payload { remaining: u64 },
}

impl Default for FrameState {
    fn default() -> Self {
        Self::Header {
            bytes: [0; FrameHeader::ENCODED_SIZE],
            filled: 0,
        }
    }
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
            frame: FrameState::default(),
            _region: region,
        }
    }

    /// Wraps an attached blocking ring reader as the read half of a
    /// peer-to-peer byte stream (see [`StreamReader::Blocking`]).
    ///
    /// The peer is considered already attached: an empty ring with no writers
    /// is EOF.
    pub fn blocking(reader: BlockingReader, region: Region) -> Self {
        Self {
            reader: StreamReader::Blocking(reader),
            frame: FrameState::default(),
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
        if buf.remaining() == 0 {
            return Poll::Ready(Ok(()));
        }

        let mut state = std::mem::take(&mut this.frame);
        // Bound the empty-frame skip so a ring full of zero-length frames
        // cannot spin the reactor indefinitely.
        for _ in 0..64 {
            match &mut state {
                FrameState::Payload { remaining } => {
                    if *remaining == 0 {
                        state = FrameState::default();
                        continue;
                    }
                    let want = (*remaining).min(buf.remaining() as u64) as usize;
                    let mut chunk = [0u8; 4096];
                    let mut chunk_buf = ReadBuf::new(
                        chunk
                            .get_mut(..want.min(4096))
                            .expect("chunk slice within the local buffer"),
                    );
                    match Pin::new(&mut this.reader).poll_read_frame(cx, &mut chunk_buf) {
                        Poll::Ready(Ok(())) => {
                            let filled = chunk_buf.filled().len();
                            if filled == 0 {
                                this.frame = state;
                                return Poll::Ready(Err(std::io::Error::new(
                                    std::io::ErrorKind::UnexpectedEof,
                                    "byte stream ended mid-frame",
                                )));
                            }
                            buf.put_slice(chunk_buf.filled());
                            *remaining -= filled as u64;
                            if *remaining == 0 {
                                state = FrameState::default();
                            }
                            this.frame = state;
                            return Poll::Ready(Ok(()));
                        }
                        Poll::Ready(Err(error)) => {
                            this.frame = state;
                            return Poll::Ready(Err(error));
                        }
                        Poll::Pending => {
                            this.frame = state;
                            return Poll::Pending;
                        }
                    }
                }
                FrameState::Header { bytes, filled } => {
                    let mut header_buf = ReadBuf::new(bytes.get_mut(*filled..).unwrap_or(&mut []));
                    match Pin::new(&mut this.reader).poll_read_frame(cx, &mut header_buf) {
                        Poll::Ready(Ok(())) => {
                            let read = header_buf.filled().len();
                            if read == 0 {
                                let eof = *filled == 0;
                                this.frame = state;
                                return if eof {
                                    Poll::Ready(Ok(()))
                                } else {
                                    Poll::Ready(Err(std::io::Error::new(
                                        std::io::ErrorKind::UnexpectedEof,
                                        "byte stream ended mid-header",
                                    )))
                                };
                            }
                            *filled += read;
                            if *filled < FrameHeader::ENCODED_SIZE {
                                continue;
                            }
                            let header = match FrameHeader::decode(bytes) {
                                Ok(header) => header,
                                Err(error) => {
                                    this.frame = state;
                                    return Poll::Ready(Err(std::io::Error::other(
                                        error.to_string(),
                                    )));
                                }
                            };
                            state = FrameState::Payload {
                                remaining: u64::from(header.len),
                            };
                        }
                        Poll::Ready(Err(error)) => {
                            this.frame = state;
                            return Poll::Ready(Err(error));
                        }
                        Poll::Pending => {
                            this.frame = state;
                            return Poll::Pending;
                        }
                    }
                }
            }
        }

        // Many zero-length frames: yield and come back for more.
        this.frame = state;
        cx.waker().wake_by_ref();
        Poll::Pending
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

        // Build the framed buffer: [FrameHeader 16 bytes][user payload].
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
    /// non-blocking writer for host-proxy protocols that do not need close or
    /// backpressure signalling. TCP uses [`attach_blocking`](Self::attach_blocking)
    /// so its writer lifetime and Park backpressure are observable.
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
        let (reader, writer) = Self::halves_from_channels_with_peer_mode(
            reader_channel,
            writer_channel,
            region,
            blocking,
            PeerMode::Attached,
        )?;
        Ok(Self { reader, writer })
    }

    /// Builds independent read/write halves from ring channels.
    ///
    /// `blocking` selects peer-to-peer semantics for **both** halves: a
    /// blocking reader (slot + wake-on-consume) and a blocking writer
    /// (writer count + EOF-to-peer). With `blocking = false` both halves
    /// are plain (host-proxy-driven rings, e.g. TCP, where the runtime's
    /// proxy threads own the peer side).
    ///
    /// The reader treats its peer as already attached; use
    /// [`halves_from_channels_with_peer_mode`](Self::halves_from_channels_with_peer_mode)
    /// for the allocator side of a per-stream channel.
    pub fn halves_from_channels(
        reader_channel: &Channel,
        writer_channel: &Channel,
        region: Region,
        blocking: bool,
    ) -> Result<(ByteStreamReader, ByteStreamWriter)> {
        Self::halves_from_channels_with_peer_mode(
            reader_channel,
            writer_channel,
            region,
            blocking,
            PeerMode::Attached,
        )
    }

    /// Builds independent read/write halves with an explicit reader peer mode.
    pub fn halves_from_channels_with_peer_mode(
        reader_channel: &Channel,
        writer_channel: &Channel,
        region: Region,
        blocking: bool,
        peer_mode: PeerMode,
    ) -> Result<(ByteStreamReader, ByteStreamWriter)> {
        let reader = if blocking {
            let reader = match peer_mode {
                PeerMode::Attached => reader_channel.blocking_reader_from(0),
                PeerMode::AwaitingFirstPeer => reader_channel.blocking_reader_from_awaiting_peer(0),
            }
            .map_err(GuestError::from)?;
            ByteStreamReader::blocking(reader, region.clone())
        } else {
            ByteStreamReader::new(reader_channel.reader(), region.clone())
        };
        let writer = if blocking {
            ByteStreamWriter::blocking(
                writer_channel.blocking_writer().map_err(GuestError::from)?,
                region,
            )
        } else {
            ByteStreamWriter::plain(writer_channel.writer().map_err(GuestError::from)?, region)
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
        let region_provider = selium_memory::region_provider()?;
        let region = region_provider.attach(shared_id, None, RegionProt::ReadWrite)?;

        // Shared byte-channel geometry parser: each direction keeps its own
        // declared capacity.
        let (inbound_channel, outbound_channel) =
            selium_shm::byte_channel::channels_from_region(&region)?;

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

#[cfg(test)]
mod tests {
    use super::*;
    use selium_abi::ResourceKind;
    use selium_memory::{HeapRegionProvider, set_region_provider};
    use selium_shm::{Channel, ChannelBackpressure};

    fn stream(capacity: u64) -> (ByteStreamReader, Channel) {
        drop(set_region_provider(Box::new(HeapRegionProvider::new())));
        let channel = Channel::create_with_backpressure(
            capacity,
            ChannelBackpressure::Park,
            ResourceKind::SharedMemory,
        )
        .expect("create channel");
        let region = selium_memory::region_provider()
            .expect("provider")
            .allocate(1, RegionProt::ReadWrite, ResourceKind::SharedMemory)
            .expect("placeholder region");
        let (reader, _writer) =
            ByteStream::halves_from_channels(&channel, &channel, region, false).expect("halves");
        (reader, channel)
    }

    fn queue_frame(channel: &Channel, payload: &[u8]) {
        let frame_size = FrameHeader::ENCODED_SIZE as u64 + payload.len() as u64;
        let pos = channel.ring().reserve(frame_size).expect("reserve");
        channel
            .ring()
            .write_frame(pos, payload, 0, 0)
            .expect("write frame");
    }

    fn read_bytes(reader: &mut ByteStreamReader, expected: usize) -> Vec<u8> {
        let mut out = Vec::with_capacity(expected);
        let waker = std::task::Waker::noop();
        let mut cx = Context::from_waker(waker);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while out.len() < expected {
            assert!(std::time::Instant::now() < deadline, "stream stalled");
            let mut chunk = [0u8; 4096];
            let mut buf = ReadBuf::new(&mut chunk);
            match Pin::new(&mut *reader).poll_read(&mut cx, &mut buf) {
                Poll::Ready(Ok(())) => {
                    let filled = buf.filled().len();
                    assert!(filled > 0, "unexpected early EOF");
                    out.extend_from_slice(buf.filled());
                }
                Poll::Ready(Err(error)) => panic!("read failed: {error}"),
                Poll::Pending => panic!("stream unexpectedly pending"),
            }
        }
        out
    }

    // scenario: guest-networking/arbitrary-frame-sizes-preserve-byte-streams/header-only-frame
    #[test]
    fn zero_length_frame_is_not_eof() {
        let (mut reader, channel) = stream(4096);
        queue_frame(&channel, b"");
        queue_frame(&channel, b"abc");
        assert_eq!(read_bytes(&mut reader, 3), b"abc");

        // The stream is now drained: the next poll is EOF (Ready with no bytes).
        let waker = std::task::Waker::noop();
        let mut cx = Context::from_waker(waker);
        let mut chunk = [0u8; 16];
        let mut buf = ReadBuf::new(&mut chunk);
        assert!(matches!(
            Pin::new(&mut reader).poll_read(&mut cx, &mut buf),
            Poll::Ready(Ok(()))
        ));
        assert!(buf.filled().is_empty());
    }

    #[test]
    fn zero_length_frame_with_live_idle_peer_parks_instead_of_eof() {
        drop(set_region_provider(Box::new(HeapRegionProvider::new())));
        let channel = Channel::create_with_backpressure(
            4096,
            ChannelBackpressure::Park,
            ResourceKind::SharedMemory,
        )
        .expect("channel");
        let region = selium_memory::region_provider()
            .expect("provider")
            .allocate(1, RegionProt::ReadWrite, ResourceKind::SharedMemory)
            .expect("placeholder");
        let (mut reader, _live_writer) =
            ByteStream::halves_from_channels(&channel, &channel, region, true).expect("halves");
        queue_frame(&channel, b"");

        let waker = std::task::Waker::noop();
        let mut cx = Context::from_waker(waker);
        let mut chunk = [0u8; 16];
        let mut buf = ReadBuf::new(&mut chunk);
        assert!(
            Pin::new(&mut reader)
                .poll_read(&mut cx, &mut buf)
                .is_pending(),
            "a header-only frame is not EOF while its writer remains attached"
        );
        assert!(buf.filled().is_empty());

        queue_frame(&channel, b"abc");
        assert_eq!(read_bytes(&mut reader, 3), b"abc");
    }

    #[tokio::test]
    async fn public_asymmetric_attach_enforces_the_outbound_ring_capacity() {
        use tokio::io::AsyncWriteExt as _;

        drop(set_region_provider(Box::new(HeapRegionProvider::new())));
        let (_inbound, _outbound, shared_id, _region) =
            selium_shm::byte_channel::create(4096, 64).expect("asymmetric channel");
        let mut stream = ByteStream::attach(shared_id).expect("public attach");
        let error = stream
            .write_all(&[0u8; 49])
            .await
            .expect_err("49 payload bytes plus the header exceed the 64-byte outbound ring");
        assert!(error.to_string().contains("buffer full"));
    }

    // scenario: guest-networking/arbitrary-frame-sizes-preserve-byte-streams/payload-exceeds-64-kib
    // scenario: guest-networking/tcp-stream-via-shared-memory-ring-buffer/frame-boundaries-are-not-messages
    #[test]
    fn large_frames_preserve_the_exact_byte_stream() {
        for payload_len in [65_521usize, 1024 * 1024] {
            let (mut reader, channel) = stream(4 * 1024 * 1024);
            let payload: Vec<u8> = (0..payload_len).map(|index| (index % 251) as u8).collect();
            queue_frame(&channel, &payload);
            assert_eq!(
                read_bytes(&mut reader, payload.len()),
                payload,
                "payload of {payload_len} bytes must survive reassembly"
            );
        }
    }
}
