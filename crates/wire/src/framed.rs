//! Framed read/write wrappers over a [`MessageTransport`].
//!
//! Uses `tokio_util::codec::FramedRead` / `FramedWrite` internally, with a
//! [`FrameCodec`] that handles [`FrameHeader`] encoding and decoding.

use std::{
    pin::Pin,
    task::{Context, Poll},
};

use bytes::{Buf, BytesMut};
use futures::{Sink, Stream};
use selium_memory::FrameHeader;
use tokio_util::codec::{
    Decoder, Encoder, FramedRead as TokioFramedRead, FramedWrite as TokioFramedWrite,
};

use crate::{
    MessageTransport,
    error::{Error, Result},
};

type FramePollResult = Poll<Option<Result<(Vec<u8>, u32, u8)>>>;

/// Codec that decodes/encodes [`FrameHeader`] + payload frames over a raw
/// byte stream.
///
/// Implements [`tokio_util::codec::Decoder`] and [`Encoder`] so that
/// [`FramedRead`] and [`FramedWrite`] can wrap any [`MessageTransport`].
///
/// The decoder returns `(payload, tag, flags)` tuples. The flags field is
/// exposed so that streaming patterns can distinguish item / end / cancel
/// frames without a second decode pass.
pub struct FrameCodec;

/// A framed reader that wraps a [`MessageTransport`] to provide frame-level
/// read operations with [`FrameHeader`] decoding and tag extraction.
pub struct FramedRead<M> {
    inner: TokioFramedRead<M, FrameCodec>,
    /// A frame that was peeked by `poll_ready` but not yet consumed.
    peeked: Option<(Vec<u8>, u32, u8)>,
}

/// A framed writer that wraps a [`MessageTransport`] to provide frame-level
/// write operations with [`FrameHeader`] encoding.
pub struct FramedWrite<M> {
    inner: TokioFramedWrite<M, FrameCodec>,
    write_state: WriteState,
}

/// State of an in-flight frame write across async polls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WriteState {
    /// No frame is buffered; the next poll starts a new frame.
    Idle,
    /// A frame was buffered by `start_send` whose transport flush is still
    /// pending; the next poll must flush it and must NOT re-send it.
    Flushing,
}

impl Decoder for FrameCodec {
    type Item = (Vec<u8>, u32, u8); // (payload, tag, flags)
    type Error = Error;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>> {
        if src.len() < FrameHeader::ENCODED_SIZE {
            return Ok(None);
        }

        let header_bytes = src
            .get(..FrameHeader::ENCODED_SIZE)
            .ok_or_else(|| Error::InvalidFrame("header bytes slice out of bounds".to_string()))?;
        let header = FrameHeader::decode(header_bytes)?;

        let frame_size = FrameHeader::ENCODED_SIZE + header.len as usize;
        if src.len() < frame_size {
            return Ok(None);
        }

        if !header.is_ready() {
            // Not yet committed — leave bytes in buffer and retry.
            return Ok(None);
        }

        let payload = src
            .get(FrameHeader::ENCODED_SIZE..frame_size)
            .ok_or_else(|| Error::InvalidFrame("payload bytes slice out of bounds".to_string()))?
            .to_vec();
        src.advance(frame_size);

        Ok(Some((payload, header.tag, header.flags)))
    }
}

impl Encoder<(Vec<u8>, u32, u8)> for FrameCodec {
    type Error = Error;

    fn encode(&mut self, item: (Vec<u8>, u32, u8), dst: &mut BytesMut) -> Result<()> {
        let (payload, tag, flags) = item;
        let header = FrameHeader {
            len: payload.len() as u32,
            tag,
            flags,
        };
        dst.reserve(FrameHeader::ENCODED_SIZE + payload.len());
        dst.extend_from_slice(&header.encode());
        dst.extend_from_slice(&payload);
        Ok(())
    }
}

impl<M: MessageTransport> FramedRead<M> {
    /// Creates a new `FramedRead` wrapping the given transport.
    pub fn new(transport: M) -> Self {
        Self {
            inner: TokioFramedRead::new(transport, FrameCodec),
            peeked: None,
        }
    }

    /// Returns a reference to the inner transport.
    pub fn inner(&self) -> &M {
        self.inner.get_ref()
    }

    /// Returns a mutable reference to the inner transport.
    pub fn inner_mut(&mut self) -> &mut M {
        self.inner.get_mut()
    }

    /// Consumes this `FramedRead` and returns the inner transport.
    pub fn into_inner(self) -> M {
        self.inner.into_inner()
    }

    /// Returns the current generation counter from the underlying transport.
    pub fn generation(&self) -> Result<u64> {
        self.inner.get_ref().generation()
    }

    /// Reads the next complete frame synchronously, returning `(payload, tag, flags)`.
    ///
    /// Returns [`Error::BufferEmpty`] if no complete frame is immediately available.
    pub fn read_frame(&mut self) -> Result<(Vec<u8>, u32, u8)> {
        // Return a previously peeked frame if present.
        if let Some(frame) = self.peeked.take() {
            return Ok(frame);
        }

        match poll_framed_read(&mut self.inner) {
            Poll::Ready(Some(item)) => item,
            Poll::Ready(None) => Err(Error::Terminated),
            Poll::Pending => Err(Error::BufferEmpty),
        }
    }

    /// Non-blocking check for frame readiness.
    ///
    /// Returns `Ok(true)` if a complete frame is immediately readable,
    /// `Ok(false)` if no frame is ready. The frame is buffered and will
    /// be returned by the next [`read_frame`](Self::read_frame) call.
    pub fn poll_ready(&mut self) -> Result<bool> {
        // If we already have a peeked frame, we're ready.
        if self.peeked.is_some() {
            return Ok(true);
        }

        // Try to read a frame from the inner reader.
        match poll_framed_read(&mut self.inner) {
            Poll::Ready(Some(Ok(frame))) => {
                self.peeked = Some(frame);
                Ok(true)
            }
            Poll::Ready(Some(Err(e))) => Err(e),
            Poll::Ready(None) => Ok(false),
            Poll::Pending => Ok(false),
        }
    }

    /// Polls for the next complete frame using the caller's task waker.
    ///
    /// The inner codec is polled with `cx`, so a `Pending` result parks the
    /// task on the underlying transport's `AsyncRead` waker (a socket read on
    /// native consumers) instead of a no-op waker or a yield loop. This is the
    /// waker-honest read path used for transports without a generation counter.
    pub fn poll_frame(&mut self, cx: &mut Context<'_>) -> Poll<Result<(Vec<u8>, u32, u8)>> {
        if let Some(frame) = self.peeked.take() {
            return Poll::Ready(Ok(frame));
        }
        match Pin::new(&mut self.inner).poll_next(cx) {
            Poll::Ready(Some(item)) => Poll::Ready(item),
            Poll::Ready(None) => Poll::Ready(Err(Error::Terminated)),
            Poll::Pending => Poll::Pending,
        }
    }

    /// Awaits the next complete frame, parking on the transport's read waker.
    ///
    /// Parks the task on the underlying transport's `AsyncRead` waker until a
    /// complete frame arrives.
    pub async fn read_frame_async(&mut self) -> Result<(Vec<u8>, u32, u8)> {
        std::future::poll_fn(|cx| self.poll_frame(cx)).await
    }

    /// Non-blocking check for peer-closed state.
    pub fn poll_peer_closed(&mut self) -> Result<bool> {
        let waker = futures::task::noop_waker();
        let mut cx = Context::from_waker(&waker);
        match Pin::new(self.inner.get_mut()).poll_peer_closed(&mut cx) {
            Poll::Ready(Ok(closed)) => Ok(closed),
            Poll::Ready(Err(e)) => Err(map_transport_error(e)),
            Poll::Pending => Ok(false),
        }
    }
}

impl<M: MessageTransport> FramedWrite<M> {
    /// Creates a new `FramedWrite` wrapping the given transport.
    pub fn new(transport: M) -> Self {
        Self {
            inner: TokioFramedWrite::new(transport, FrameCodec),
            write_state: WriteState::Idle,
        }
    }

    /// Returns a reference to the inner transport.
    pub fn inner(&self) -> &M {
        self.inner.get_ref()
    }

    /// Returns a mutable reference to the inner transport.
    pub fn inner_mut(&mut self) -> &mut M {
        self.inner.get_mut()
    }

    /// Consumes this `FramedWrite` and returns the inner transport.
    pub fn into_inner(self) -> M {
        self.inner.into_inner()
    }

    /// Writes a framed payload with the given correlation tag synchronously.
    ///
    /// Encodes a [`FrameHeader`] with the payload length and tag, writes
    /// the frame to the underlying transport with `FLAG_READY` set.
    pub fn write_frame(&mut self, payload: &[u8], tag: u32) -> Result<()> {
        self.write_frame_with_flags(payload, tag, FrameHeader::FLAG_READY)
    }

    /// Writes a framed payload with the given correlation tag and custom flags.
    ///
    /// Synchronous best-effort variant: polls the transport with a no-op waker,
    /// so a full ring surfaces as [`Error::BufferFull`] instead of parking.
    /// Streaming senders that should park on backpressure use
    /// [`write_frame_with_flags_async`](Self::write_frame_with_flags_async)
    /// instead.
    pub fn write_frame_with_flags(&mut self, payload: &[u8], tag: u32, flags: u8) -> Result<()> {
        let waker = futures::task::noop_waker();
        let mut cx = Context::from_waker(&waker);
        match self.poll_write_frame_with_flags(&mut cx, payload, tag, flags) {
            Poll::Ready(result) => result,
            // The sync path never registered a real waker, so Pending here
            // means "would block" — surface it as BufferFull.
            Poll::Pending => {
                // A frame may have been buffered by start_send before the
                // flush pended; flush it on the next write attempt.
                Err(Error::BufferFull)
            }
        }
    }

    /// Writes a framed payload with the given correlation tag and custom flags,
    /// parking on a full ring until capacity frees.
    ///
    /// This is the backpressure-honest write path used by the streaming
    /// patterns: when the underlying transport reports pending (Park channel
    /// with a full ring), the returned future yields [`Poll::Pending`] after
    /// registering the task's waker via the generation-wait mechanism, and is
    /// re-polled once capacity frees. No data is dropped and no bounded retry
    /// loop spins at the call site.
    ///
    /// # Errors
    ///
    /// Returns an error if the transport fails or the frame exceeds `u32::MAX`.
    pub async fn write_frame_with_flags_async(
        &mut self,
        payload: &[u8],
        tag: u32,
        flags: u8,
    ) -> Result<()> {
        std::future::poll_fn(|cx| self.poll_write_frame_with_flags(cx, payload, tag, flags)).await
    }

    /// Poll-based core for frame writes; safe to drive from a real waker.
    fn poll_write_frame_with_flags(
        &mut self,
        cx: &mut Context<'_>,
        payload: &[u8],
        tag: u32,
        flags: u8,
    ) -> Poll<Result<()>> {
        if payload.len() > u32::MAX as usize {
            return Poll::Ready(Err(Error::InvalidFrame(format!(
                "payload length {} exceeds u32::MAX",
                payload.len()
            ))));
        }

        // Complete an in-flight frame's flush first. Crucially, completing it
        // finishes THIS write: the frame was already buffered by start_send,
        // so we must not fall through and buffer it a second time.
        if self.write_state == WriteState::Flushing {
            self.write_state = WriteState::Idle;
            return match Pin::new(&mut self.inner).poll_flush(cx) {
                Poll::Ready(Ok(())) => Poll::Ready(Ok(())),
                Poll::Ready(Err(e)) => Poll::Ready(Err(map_transport_error(e))),
                Poll::Pending => {
                    self.write_state = WriteState::Flushing;
                    Poll::Pending
                }
            };
        }

        // Reserve capacity on the transport. On a Park channel with a full
        // ring this registers the generation-wait waker and returns Pending.
        match Pin::new(&mut self.inner).poll_ready(cx) {
            Poll::Ready(Ok(())) => {}
            Poll::Ready(Err(e)) => return Poll::Ready(Err(map_transport_error(e))),
            Poll::Pending => return Poll::Pending,
        }

        // Encode the item into the internal buffer.
        Pin::new(&mut self.inner).start_send((payload.to_vec(), tag, flags))?;

        // Flush to the underlying transport.
        match Pin::new(&mut self.inner).poll_flush(cx) {
            Poll::Ready(Ok(())) => Poll::Ready(Ok(())),
            Poll::Ready(Err(e)) => Poll::Ready(Err(map_transport_error(e))),
            Poll::Pending => {
                self.write_state = WriteState::Flushing;
                Poll::Pending
            }
        }
    }
}

fn map_transport_error<E: std::error::Error>(err: E) -> Error {
    if let Some(our_err) = err.source().and_then(|e| e.downcast_ref::<Error>()) {
        return our_err.clone();
    }
    Error::Transport(err.to_string())
}

/// Polls a `TokioFramedRead` once with a noop waker for synchronous use.
fn poll_framed_read<M>(framed: &mut TokioFramedRead<M, FrameCodec>) -> FramePollResult
where
    M: MessageTransport,
{
    let waker = futures::task::noop_waker();
    let mut cx = Context::from_waker(&waker);
    Pin::new(framed).poll_next(&mut cx)
}
