//! Transport-agnostic streaming RPC patterns.
//!
//! This module provides server-streaming and bidi-streaming RPC handles
//! generic over [`MessageTransport`]. All streaming frames share the
//! correlation tag assigned by the client at call time, and distinguish
//! stream lifecycle events via [`FrameHeader`] flags.

use std::{
    marker::PhantomData,
    pin::Pin,
    task::{Context, Poll},
};

use futures::Stream;
use selium_memory::FrameHeader;
use selium_service::FlatMsg;

use crate::{
    MessageTransport,
    error::{Error, Result as WireResult},
    framed::{FramedRead, FramedWrite},
    rpc::RpcError,
};

/// Client-side handle for server-streaming RPC: one request, ordered stream
/// of reply items.
pub struct RpcServerStreamClient<Req, Item, M> {
    request_writer: FramedWrite<M>,
    reply_reader: FramedRead<M>,
    next_correlation: u32,
    _phantom: PhantomData<(Req, Item)>,
}

/// A live server-stream received in response to a single request.
///
/// Implements [`Stream`] yielding `Result<Item, RpcError>`. The stream
/// terminates naturally when the server sends an end frame, or early if a
/// cancel frame is sent by [`cancel`](Self::cancel) (or via [`Drop`]).
///
/// Dropping this handle before the stream is exhausted sends a cancel frame
/// to the server.
pub struct RpcServerStream<'a, Item, M: MessageTransport> {
    reply_reader: &'a mut FramedRead<M>,
    request_writer: &'a mut FramedWrite<M>,
    correlation: u32,
    done: bool,
    _phantom: PhantomData<Item>,
}

/// Server-side handle for an established server-streaming RPC session.
pub struct RpcServerStreamConnection<Req, Item, M> {
    request_reader: FramedRead<M>,
    reply_writer: FramedWrite<M>,
    client_process_id: u64,
    _phantom: PhantomData<(Req, Item)>,
}

/// A server-streaming request received from a client.
///
/// The server can [`send_item`](Self::send_item) repeatedly, then call
/// [`finish`](Self::finish) to signal end-of-stream, or
/// [`send_error`](Self::send_error) to terminate with an error.
///
/// Between items the server should call [`check_cancel`](Self::check_cancel)
/// to see whether the client has cancelled the stream.
pub struct RpcServerStreamRequest<'a, Req, Item, M: MessageTransport> {
    reply_writer: &'a mut FramedWrite<M>,
    request_reader: &'a mut FramedRead<M>,
    payload_bytes: Vec<u8>,
    correlation: u32,
    finished: bool,
    _phantom: PhantomData<(Req, Item)>,
}

/// Client-side handle for bidi-streaming RPC: one request, independent
/// send and receive streams over a single correlation tag.
pub struct RpcBidiStreamClient<Req, Item, Resp, M> {
    request_writer: FramedWrite<M>,
    reply_reader: FramedRead<M>,
    next_correlation: u32,
    _phantom: PhantomData<(Req, Item, Resp)>,
}

/// An established bidi-streaming session on the client side.
///
/// Provides independent send and receive halves that share one correlation
/// tag. Each direction can be closed independently via its own end flag
/// (half-close semantics, mirroring TCP half-close).
pub struct RpcBidiStream<'a, Item, Resp, M: MessageTransport> {
    request_writer: &'a mut FramedWrite<M>,
    reply_reader: &'a mut FramedRead<M>,
    correlation: u32,
    send_done: bool,
    recv_done: bool,
    _phantom: PhantomData<(Item, Resp)>,
}

/// The send half of a bidi stream, obtained via [`RpcBidiStream::split`].
pub struct BidiSender<'a, Item, M: MessageTransport> {
    writer: &'a mut FramedWrite<M>,
    correlation: u32,
    done: bool,
    _phantom: PhantomData<Item>,
}

/// The receive half of a bidi stream, obtained via [`RpcBidiStream::split`].
pub struct BidiReceiver<'a, Resp, M: MessageTransport> {
    reader: &'a mut FramedRead<M>,
    correlation: u32,
    done: bool,
    _phantom: PhantomData<Resp>,
}

/// Server-side handle for an established bidi-streaming RPC session.
pub struct RpcBidiStreamConnection<Req, Item, Resp, M> {
    request_reader: FramedRead<M>,
    reply_writer: FramedWrite<M>,
    client_process_id: u64,
    _phantom: PhantomData<(Req, Item, Resp)>,
}

/// A bidi-streaming request received from a client.
///
/// Provides a [`split`](Self::split) method to obtain independent send
/// and receive halves, each of which can be closed independently.
pub struct RpcBidiStreamRequest<'a, Req, Item, Resp, M: MessageTransport> {
    reply_writer: &'a mut FramedWrite<M>,
    request_reader: &'a mut FramedRead<M>,
    payload_bytes: Vec<u8>,
    correlation: u32,
    _phantom: PhantomData<(Req, Item, Resp)>,
}

/// Server-side send half of a bidi stream.
pub struct BidiResponder<'a, Resp, M: MessageTransport> {
    writer: &'a mut FramedWrite<M>,
    correlation: u32,
    done: bool,
    _phantom: PhantomData<Resp>,
}

/// Server-side receive half of a bidi stream.
pub struct BidiRequestStream<'a, Item, M: MessageTransport> {
    reader: &'a mut FramedRead<M>,
    correlation: u32,
    done: bool,
    _phantom: PhantomData<Item>,
}

impl<Req, Item, M> RpcServerStreamClient<Req, Item, M>
where
    M: MessageTransport,
{
    /// Creates a new server-streaming RPC client from pre-established
    /// request and reply transports.
    pub fn new(request_writer: FramedWrite<M>, reply_reader: FramedRead<M>) -> Self {
        Self {
            request_writer,
            reply_reader,
            next_correlation: 1,
            _phantom: PhantomData,
        }
    }
}

impl<Req, Item, M> RpcServerStreamClient<Req, Item, M>
where
    Req: FlatMsg,
    Item: FlatMsg,
    M: MessageTransport,
{
    /// Sends a typed request and returns a [`Stream`] of reply items.
    ///
    /// Only one stream may be active at a time — the returned
    /// [`RpcServerStream`] holds a mutable borrow on the reply channel.
    ///
    /// # Errors
    ///
    /// Returns [`RpcError::BufferFull`] if the request cannot be written
    /// immediately.
    pub async fn call(
        &mut self,
        req: Req,
    ) -> std::result::Result<RpcServerStream<'_, Item, M>, RpcError> {
        let correlation = self.next_correlation;
        self.next_correlation = self.next_correlation.wrapping_add(1);

        let encoded = FlatMsg::encode(&req);
        self.request_writer.write_frame(&encoded, correlation)?;

        Ok(RpcServerStream {
            reply_reader: &mut self.reply_reader,
            request_writer: &mut self.request_writer,
            correlation,
            done: false,
            _phantom: PhantomData,
        })
    }
}

impl<Item, M> RpcServerStream<'_, Item, M>
where
    M: MessageTransport,
{
    /// Sends a cancel frame to the server and marks the stream as done.
    ///
    /// After calling this, the stream will yield `None` on the next poll.
    /// Calling `cancel` on an already-done stream is harmless.
    pub fn cancel(&mut self) {
        if !self.done {
            self.done = true;
            // Best-effort: if the write fails the stream is already terminating.
            let cancel_flags = FrameHeader::FLAG_READY | FrameHeader::FLAG_STREAM_CANCEL;
            drop(
                self.request_writer
                    .write_frame_with_flags(&[], self.correlation, cancel_flags),
            );
        }
    }
}

impl<Item, M> Stream for RpcServerStream<'_, Item, M>
where
    Item: FlatMsg + Unpin,
    M: MessageTransport,
{
    type Item = std::result::Result<Item, RpcError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        if this.done {
            return Poll::Ready(None);
        }

        // Socket transports (no generation counter) park on the transport's
        // read waker; rings use the generation-based read below.
        if this.reply_reader.inner().region_id() == 0 {
            loop {
                match this.reply_reader.poll_frame(cx) {
                    Poll::Ready(Ok((payload, tag, flags))) => {
                        if tag != this.correlation {
                            // Invariant: only one stream is active per client at
                            // a time, so a foreign tag indicates a peer bug;
                            // skip it and keep waiting for our tag.
                            continue;
                        }

                        if FrameHeader::FLAG_STREAM_CANCEL & flags != 0 {
                            this.done = true;
                            return Poll::Ready(None);
                        }

                        if FrameHeader::FLAG_STREAM_ERROR & flags != 0 {
                            this.done = true;
                            let message = String::from_utf8_lossy(&payload).into_owned();
                            return Poll::Ready(Some(Err(RpcError::Remote(message))));
                        }

                        let is_end = FrameHeader::FLAG_STREAM_END & flags != 0;
                        if is_end {
                            this.done = true;
                        }
                        if payload.is_empty() && is_end {
                            return Poll::Ready(None);
                        }

                        return match FlatMsg::decode(&payload) {
                            Ok(item) => Poll::Ready(Some(Ok(item))),
                            Err(e) => {
                                Poll::Ready(Some(Err(RpcError::Serialization(format!("{e}")))))
                            }
                        };
                    }
                    Poll::Ready(Err(Error::Terminated)) => {
                        this.done = true;
                        return Poll::Ready(None);
                    }
                    Poll::Ready(Err(e)) => return Poll::Ready(Some(Err(e.into()))),
                    Poll::Pending => return Poll::Pending,
                }
            }
        }

        match try_poll_ready(this.reply_reader) {
            Ok(true) => {
                match this.reply_reader.read_frame() {
                    Ok((payload, tag, flags)) => {
                        if tag != this.correlation {
                            // Invariant: only one stream is active per client
                            // at a time (the stream borrows the reply channel
                            // exclusively), so every reply frame must carry
                            // the active correlation tag. A mismatch therefore
                            // indicates a peer bug; the frame is skipped and
                            // we keep waiting for our tag.
                            register_gen_wait(this.reply_reader, cx);
                            return Poll::Pending;
                        }

                        // Check for a cancel frame from the server. The server
                        // sends this when it abandons the stream without
                        // completing it (e.g. its request handle was dropped),
                        // so the client observes termination deterministically
                        // instead of via peer-close detection.
                        if FrameHeader::FLAG_STREAM_CANCEL & flags != 0 {
                            this.done = true;
                            return Poll::Ready(None);
                        }

                        // Check for a mid-stream error frame: the payload is
                        // the server's error message, not a typed item.
                        if FrameHeader::FLAG_STREAM_ERROR & flags != 0 {
                            this.done = true;
                            let message = String::from_utf8_lossy(&payload).into_owned();
                            return Poll::Ready(Some(Err(RpcError::Remote(message))));
                        }

                        let is_end = FrameHeader::FLAG_STREAM_END & flags != 0;

                        if is_end {
                            this.done = true;
                        }

                        // An end-only frame with no payload completes the stream
                        // without yielding an item.
                        if payload.is_empty() && is_end {
                            return Poll::Ready(None);
                        }

                        match FlatMsg::decode(&payload) {
                            Ok(item) => Poll::Ready(Some(Ok(item))),
                            Err(e) => {
                                Poll::Ready(Some(Err(RpcError::Serialization(format!("{e}")))))
                            }
                        }
                    }
                    Err(Error::BufferEmpty) => {
                        register_gen_wait(this.reply_reader, cx);
                        Poll::Pending
                    }
                    Err(e) => Poll::Ready(Some(Err(e.into()))),
                }
            }
            Ok(false) => {
                register_gen_wait(this.reply_reader, cx);
                Poll::Pending
            }
            Err(e) => Poll::Ready(Some(Err(e.into()))),
        }
    }
}

impl<Item, M: MessageTransport> Drop for RpcServerStream<'_, Item, M> {
    fn drop(&mut self) {
        if !self.done {
            self.cancel();
        }
    }
}

impl<Req, Item, M> RpcServerStreamConnection<Req, Item, M>
where
    M: MessageTransport,
{
    /// Creates a new server-streaming RPC connection from pre-established
    /// request and reply transports.
    pub fn new(
        request_reader: FramedRead<M>,
        reply_writer: FramedWrite<M>,
        client_process_id: u64,
    ) -> Self {
        Self {
            request_reader,
            reply_writer,
            client_process_id,
            _phantom: PhantomData,
        }
    }

    /// Returns the process id of the connected client.
    pub fn client_process_id(&self) -> u64 {
        self.client_process_id
    }
}

impl<Req, Item, M> RpcServerStreamConnection<Req, Item, M>
where
    Req: FlatMsg,
    Item: FlatMsg,
    M: MessageTransport,
{
    /// Receives the next streaming request from the client.
    ///
    /// Stale stream lifecycle frames (cancel/end) left over from a previous
    /// stream on this connection are drained and skipped, so a cancelled or
    /// dropped stream never surfaces as a bogus new request.
    pub async fn recv(
        &mut self,
    ) -> std::result::Result<RpcServerStreamRequest<'_, Req, Item, M>, RpcError> {
        // Socket transports park on the transport's read waker.
        if self.request_reader.inner().region_id() == 0 {
            loop {
                let (payload_bytes, correlation, flags) =
                    match self.request_reader.read_frame_async().await {
                        Ok(frame) => frame,
                        Err(Error::Terminated) => return Err(RpcError::ConnectionClosed),
                        Err(e) => return Err(e.into()),
                    };

                // Skip stale lifecycle frames from previous streams: genuine
                // requests never carry stream flags.
                let lifecycle = FrameHeader::FLAG_STREAM_CANCEL
                    | FrameHeader::FLAG_STREAM_END
                    | FrameHeader::FLAG_STREAM_ITEM;
                if flags & lifecycle != 0 {
                    continue;
                }

                return Ok(RpcServerStreamRequest {
                    reply_writer: &mut self.reply_writer,
                    request_reader: &mut self.request_reader,
                    payload_bytes,
                    correlation,
                    finished: false,
                    _phantom: PhantomData,
                });
            }
        }

        let region_id = self.request_reader.inner().region_id();
        let mut last_generation = self
            .request_reader
            .generation()
            .unwrap_or(0)
            .wrapping_sub(1);

        loop {
            let current_generation = self.request_reader.generation().unwrap_or(0);

            if current_generation != last_generation {
                last_generation = current_generation;

                match self.request_reader.read_frame() {
                    Ok((payload_bytes, correlation, flags)) => {
                        // Skip stale lifecycle frames from previous streams
                        // (cancel, end, or undelivered session items): genuine
                        // requests never carry stream flags.
                        let lifecycle = FrameHeader::FLAG_STREAM_CANCEL
                            | FrameHeader::FLAG_STREAM_END
                            | FrameHeader::FLAG_STREAM_ITEM;
                        if flags & lifecycle != 0 {
                            continue;
                        }
                        return Ok(RpcServerStreamRequest {
                            reply_writer: &mut self.reply_writer,
                            request_reader: &mut self.request_reader,
                            payload_bytes,
                            correlation,
                            finished: false,
                            _phantom: PhantomData,
                        });
                    }
                    Err(Error::BufferEmpty) => {}
                    Err(e) => return Err(e.into()),
                }
            }

            if self.request_reader.poll_peer_closed()? {
                return Err(RpcError::ConnectionClosed);
            }

            if region_id != 0 {
                crate::generation_wait(region_id, last_generation).await;
            }
        }
    }
}

impl<'a, Req, Item, M> RpcServerStreamRequest<'a, Req, Item, M>
where
    Req: FlatMsg,
    Item: FlatMsg,
    M: MessageTransport,
{
    /// Returns a reference to the raw request payload bytes.
    pub fn payload_bytes(&self) -> &[u8] {
        &self.payload_bytes
    }

    /// Decodes the request payload.
    pub fn payload(&self) -> std::result::Result<Req, RpcError> {
        Req::decode(&self.payload_bytes)
            .map_err(|e| RpcError::Serialization(format!("decode request: {e}")))
    }

    /// Decodes and returns the request payload by value.
    pub fn into_payload(self) -> std::result::Result<Req, RpcError> {
        self.payload()
    }

    /// Returns the correlation tag for this stream.
    pub fn correlation(&self) -> u32 {
        self.correlation
    }

    /// Sends a stream item to the client.
    ///
    /// Parks on a full ring via the generation-wait mechanism (backpressure
    /// honesty); no items are dropped or buffered without bound.
    ///
    /// # Errors
    ///
    /// Returns [`RpcError::ConnectionClosed`] if the stream is already
    /// finished, or a transport error if the write fails.
    pub async fn send_item(&mut self, item: Item) -> std::result::Result<(), RpcError>
    where
        Item: FlatMsg,
    {
        if self.finished {
            return Err(RpcError::ConnectionClosed);
        }

        let encoded = FlatMsg::encode(&item);
        let flags = FrameHeader::FLAG_READY | FrameHeader::FLAG_STREAM_ITEM;
        self.reply_writer
            .write_frame_with_flags_async(&encoded, self.correlation, flags)
            .await?;
        Ok(())
    }

    /// Sends the final stream item and marks the stream as ended.
    ///
    /// Combines `FLAG_STREAM_ITEM` and `FLAG_STREAM_END` in a single frame.
    pub async fn send_final_item(&mut self, item: Item) -> std::result::Result<(), RpcError>
    where
        Item: FlatMsg,
    {
        if self.finished {
            return Err(RpcError::ConnectionClosed);
        }
        self.finished = true;

        let encoded = FlatMsg::encode(&item);
        let flags =
            FrameHeader::FLAG_READY | FrameHeader::FLAG_STREAM_ITEM | FrameHeader::FLAG_STREAM_END;
        self.reply_writer
            .write_frame_with_flags_async(&encoded, self.correlation, flags)
            .await?;
        Ok(())
    }

    /// Signals end-of-stream with no final payload item.
    pub async fn finish(&mut self) -> std::result::Result<(), RpcError> {
        if self.finished {
            return Ok(());
        }
        self.finished = true;

        let flags = FrameHeader::FLAG_READY | FrameHeader::FLAG_STREAM_END;
        self.reply_writer
            .write_frame_with_flags_async(&[], self.correlation, flags)
            .await?;
        Ok(())
    }

    /// Sends an application error and terminates the stream.
    ///
    /// The frame carries `FLAG_STREAM_ERROR | FLAG_STREAM_END` with the error
    /// message as its payload; the client's stream yields
    /// [`RpcError::Remote`] carrying this message, preserving the server-side
    /// failure semantics across the stream boundary.
    pub async fn send_error(
        &mut self,
        message: impl Into<String>,
    ) -> std::result::Result<(), RpcError> {
        if self.finished {
            return Err(RpcError::ConnectionClosed);
        }
        self.finished = true;

        let payload = message.into().into_bytes();
        let flags =
            FrameHeader::FLAG_READY | FrameHeader::FLAG_STREAM_ERROR | FrameHeader::FLAG_STREAM_END;
        self.reply_writer
            .write_frame_with_flags_async(&payload, self.correlation, flags)
            .await?;
        Ok(())
    }

    /// Checks whether the client has sent a cancel frame for this stream.
    ///
    /// Returns `true` if a cancel was received. The server should stop
    /// producing items and release per-stream resources.
    ///
    /// This is a non-blocking poll — call between `send_item` calls. Any
    /// non-cancel frame observed here is stale lifecycle traffic from a
    /// previous stream (the request direction carries nothing else while this
    /// stream is active) and is drained silently.
    pub fn check_cancel(&mut self) -> bool {
        if self.finished {
            return true;
        }

        // Try to peek for a cancel frame without blocking.
        if self.request_reader.poll_ready() == Ok(true)
            && let Ok((_payload, tag, flags)) = self.request_reader.read_frame()
            && FrameHeader::FLAG_STREAM_CANCEL & flags != 0
            && tag == self.correlation
        {
            // Non-cancel frames, or cancels for a foreign/completed tag:
            // harmless per the design (cancel-after-end is ignored).
            self.finished = true;
            return true;
        }

        false
    }
}

impl<Req, Item, M: MessageTransport> Drop for RpcServerStreamRequest<'_, Req, Item, M> {
    fn drop(&mut self) {
        // Mirror of the client-side drop-cancel: if the server abandons an
        // unfinished stream, notify the client deterministically with a
        // cancel frame instead of letting it wait for peer-close detection.
        // Best-effort: on a full ring the write is skipped rather than
        // blocking inside drop; the client still terminates via peer-close.
        if !self.finished {
            let flags = FrameHeader::FLAG_READY | FrameHeader::FLAG_STREAM_CANCEL;
            drop(
                self.reply_writer
                    .write_frame_with_flags(&[], self.correlation, flags),
            );
            self.finished = true;
        }
    }
}

impl<Req, Item, Resp, M> RpcBidiStreamClient<Req, Item, Resp, M>
where
    M: MessageTransport,
{
    /// Creates a new bidi-streaming RPC client from pre-established
    /// request and reply transports.
    pub fn new(request_writer: FramedWrite<M>, reply_reader: FramedRead<M>) -> Self {
        Self {
            request_writer,
            reply_reader,
            next_correlation: 1,
            _phantom: PhantomData,
        }
    }
}

impl<Req, Item, Resp, M> RpcBidiStreamClient<Req, Item, Resp, M>
where
    Req: FlatMsg,
    Item: FlatMsg,
    Resp: FlatMsg,
    M: MessageTransport,
{
    /// Sends the opening request and returns a [`RpcBidiStream`] for the
    /// session.
    ///
    /// Only one session may be active at a time — the returned handle
    /// holds mutable borrows on both channels.
    pub async fn connect(
        &mut self,
        req: Req,
    ) -> std::result::Result<RpcBidiStream<'_, Item, Resp, M>, RpcError> {
        let correlation = self.next_correlation;
        self.next_correlation = self.next_correlation.wrapping_add(1);

        let encoded = FlatMsg::encode(&req);
        self.request_writer.write_frame(&encoded, correlation)?;

        Ok(RpcBidiStream {
            request_writer: &mut self.request_writer,
            reply_reader: &mut self.reply_reader,
            correlation,
            send_done: false,
            recv_done: false,
            _phantom: PhantomData,
        })
    }
}

impl<Item, Resp, M: MessageTransport> RpcBidiStream<'_, Item, Resp, M> {
    /// Returns the correlation tag for this session.
    pub fn correlation(&self) -> u32 {
        self.correlation
    }

    /// Splits the bidi stream into independent send and receive halves.
    ///
    /// Each half can be used independently; closing one direction
    /// (via [`BidiSender::close`] or dropping the sender) does not
    /// affect the other.
    pub fn split(&mut self) -> (BidiSender<'_, Item, M>, BidiReceiver<'_, Resp, M>) {
        (
            BidiSender {
                writer: self.request_writer,
                correlation: self.correlation,
                done: self.send_done,
                _phantom: PhantomData,
            },
            BidiReceiver {
                reader: self.reply_reader,
                correlation: self.correlation,
                done: self.recv_done,
                _phantom: PhantomData,
            },
        )
    }
}

impl<Item, Resp, M: MessageTransport> Drop for RpcBidiStream<'_, Item, Resp, M> {
    fn drop(&mut self) {
        // Send end-of-stream on the send direction if not already closed.
        if !self.send_done {
            self.send_done = true;
            let flags = FrameHeader::FLAG_READY | FrameHeader::FLAG_STREAM_END;
            drop(
                self.request_writer
                    .write_frame_with_flags(&[], self.correlation, flags),
            );
        }
    }
}

impl<Item, M: MessageTransport> BidiSender<'_, Item, M> {
    /// Sends a stream item on the send direction.
    ///
    /// Parks on a full ring via the generation-wait mechanism (backpressure
    /// honesty).
    ///
    /// # Errors
    ///
    /// Returns [`RpcError::ConnectionClosed`] if the send direction is
    /// already closed, or a transport error if the write fails.
    pub async fn send(&mut self, item: Item) -> std::result::Result<(), RpcError>
    where
        Item: FlatMsg,
    {
        if self.done {
            return Err(RpcError::ConnectionClosed);
        }

        let encoded = FlatMsg::encode(&item);
        let flags = FrameHeader::FLAG_READY | FrameHeader::FLAG_STREAM_ITEM;
        self.writer
            .write_frame_with_flags_async(&encoded, self.correlation, flags)
            .await?;
        Ok(())
    }

    /// Sends the final item and closes the send direction.
    ///
    /// After this call further `send` attempts will return
    /// [`RpcError::ConnectionClosed`].
    pub async fn close_with_item(&mut self, item: Item) -> std::result::Result<(), RpcError>
    where
        Item: FlatMsg,
    {
        if self.done {
            return Err(RpcError::ConnectionClosed);
        }
        self.done = true;

        let encoded = FlatMsg::encode(&item);
        let flags =
            FrameHeader::FLAG_READY | FrameHeader::FLAG_STREAM_ITEM | FrameHeader::FLAG_STREAM_END;
        self.writer
            .write_frame_with_flags_async(&encoded, self.correlation, flags)
            .await?;
        Ok(())
    }

    /// Closes the send direction with no final payload.
    ///
    /// The peer will observe end-of-stream on its receive half upon
    /// receiving the end flag.
    pub async fn close(&mut self) -> std::result::Result<(), RpcError> {
        if self.done {
            return Ok(());
        }
        self.done = true;

        let flags = FrameHeader::FLAG_READY | FrameHeader::FLAG_STREAM_END;
        self.writer
            .write_frame_with_flags_async(&[], self.correlation, flags)
            .await?;
        Ok(())
    }

    /// Returns `true` if the send direction has been closed.
    pub fn is_closed(&self) -> bool {
        self.done
    }
}

impl<Item, M: MessageTransport> Drop for BidiSender<'_, Item, M> {
    fn drop(&mut self) {
        if !self.done {
            self.done = true;
            let flags = FrameHeader::FLAG_READY | FrameHeader::FLAG_STREAM_END;
            drop(
                self.writer
                    .write_frame_with_flags(&[], self.correlation, flags),
            );
        }
    }
}

impl<Resp, M: MessageTransport> BidiReceiver<'_, Resp, M> {
    /// Receives the next stream item, or `None` if the receive direction
    /// has been closed (end flag received).
    ///
    /// This is a non-blocking poll — returns `Ok(None)` if no frame is
    /// immediately available.
    pub fn try_recv(&mut self) -> std::result::Result<Option<Resp>, RpcError>
    where
        Resp: FlatMsg,
    {
        if self.done {
            return Ok(None);
        }

        match try_poll_ready(self.reader) {
            Ok(true) => {}
            Ok(false) => return Ok(None),
            Err(e) => return Err(e.into()),
        }

        match self.reader.read_frame() {
            Ok((payload, tag, flags)) => {
                if tag != self.correlation {
                    // Invariant: one active bidi session per connection pair;
                    // a foreign tag indicates a peer bug. Report "no data
                    // available" rather than misinterpreting the frame.
                    return Ok(None);
                }

                // Server abandoned its reply direction for this session.
                if FrameHeader::FLAG_STREAM_CANCEL & flags != 0 {
                    self.done = true;
                    return Ok(None);
                }

                // Mid-stream error from the server: payload is the message.
                if FrameHeader::FLAG_STREAM_ERROR & flags != 0 {
                    self.done = true;
                    let message = String::from_utf8_lossy(&payload).into_owned();
                    return Err(RpcError::Remote(message));
                }

                if FrameHeader::FLAG_STREAM_END & flags != 0 {
                    self.done = true;
                }

                if payload.is_empty() && FrameHeader::FLAG_STREAM_END & flags != 0 {
                    return Ok(None);
                }

                FlatMsg::decode(&payload)
                    .map(Some)
                    .map_err(|e| RpcError::Serialization(format!("{e}")))
            }
            Err(Error::BufferEmpty) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Asynchronously receives the next stream item.
    ///
    /// Parks via generation-wait if no frame is ready. Returns `None`
    /// when the send direction has been closed (end-of-stream).
    pub async fn recv(&mut self) -> std::result::Result<Option<Resp>, RpcError>
    where
        Resp: FlatMsg,
    {
        // Socket transports park on the transport's read waker.
        if self.reader.inner().region_id() == 0 {
            loop {
                if self.done {
                    return Ok(None);
                }

                let (payload, tag, flags) = match self.reader.read_frame_async().await {
                    Ok(frame) => frame,
                    Err(Error::Terminated) => return Err(RpcError::ConnectionClosed),
                    Err(e) => return Err(e.into()),
                };

                if tag != self.correlation {
                    // Invariant: one active bidi session per connection pair;
                    // skip foreign frames.
                    continue;
                }

                if FrameHeader::FLAG_STREAM_CANCEL & flags != 0 {
                    self.done = true;
                    return Ok(None);
                }

                if FrameHeader::FLAG_STREAM_ERROR & flags != 0 {
                    self.done = true;
                    let message = String::from_utf8_lossy(&payload).into_owned();
                    return Err(RpcError::Remote(message));
                }

                let is_end = FrameHeader::FLAG_STREAM_END & flags != 0;
                if is_end {
                    self.done = true;
                }
                if payload.is_empty() && is_end {
                    return Ok(None);
                }

                return FlatMsg::decode(&payload)
                    .map(Some)
                    .map_err(|e| RpcError::Serialization(format!("{e}")));
            }
        }

        let region_id = self.reader.inner().region_id();
        let mut last_generation = self.reader.generation().unwrap_or(0).wrapping_sub(1);

        loop {
            if self.done {
                return Ok(None);
            }

            let current_generation = self.reader.generation().unwrap_or(0);

            if current_generation != last_generation {
                last_generation = current_generation;

                match self.reader.read_frame() {
                    Ok((payload, tag, flags)) => {
                        if tag != self.correlation {
                            // Invariant: one active bidi session per
                            // connection pair; skip foreign frames.
                            continue;
                        }

                        if FrameHeader::FLAG_STREAM_CANCEL & flags != 0 {
                            self.done = true;
                            return Ok(None);
                        }

                        if FrameHeader::FLAG_STREAM_ERROR & flags != 0 {
                            self.done = true;
                            let message = String::from_utf8_lossy(&payload).into_owned();
                            return Err(RpcError::Remote(message));
                        }

                        let is_end = FrameHeader::FLAG_STREAM_END & flags != 0;

                        if is_end {
                            self.done = true;
                        }

                        if payload.is_empty() && is_end {
                            return Ok(None);
                        }

                        return FlatMsg::decode(&payload)
                            .map(Some)
                            .map_err(|e| RpcError::Serialization(format!("{e}")));
                    }
                    Err(Error::BufferEmpty) => {}
                    Err(e) => return Err(e.into()),
                }
            }

            if self.reader.poll_peer_closed()? {
                return Err(RpcError::ConnectionClosed);
            }

            if region_id != 0 {
                crate::generation_wait(region_id, last_generation).await;
            }
        }
    }

    /// Returns `true` if the receive direction has been closed.
    pub fn is_closed(&self) -> bool {
        self.done
    }
}

impl<Req, Item, Resp, M> RpcBidiStreamConnection<Req, Item, Resp, M>
where
    M: MessageTransport,
{
    /// Creates a new bidi-streaming RPC connection from pre-established
    /// request and reply transports.
    pub fn new(
        request_reader: FramedRead<M>,
        reply_writer: FramedWrite<M>,
        client_process_id: u64,
    ) -> Self {
        Self {
            request_reader,
            reply_writer,
            client_process_id,
            _phantom: PhantomData,
        }
    }

    /// Returns the process id of the connected client.
    pub fn client_process_id(&self) -> u64 {
        self.client_process_id
    }
}

impl<Req, Item, Resp, M> RpcBidiStreamConnection<Req, Item, Resp, M>
where
    Req: FlatMsg,
    Item: FlatMsg,
    Resp: FlatMsg,
    M: MessageTransport,
{
    /// Receives the next bidi-streaming request from the client.
    ///
    /// Stale stream lifecycle frames (cancel/end/items) left over from a
    /// previous session on this connection are drained and skipped.
    pub async fn recv(
        &mut self,
    ) -> std::result::Result<RpcBidiStreamRequest<'_, Req, Item, Resp, M>, RpcError> {
        // Socket transports park on the transport's read waker.
        if self.request_reader.inner().region_id() == 0 {
            loop {
                let (payload_bytes, correlation, flags) =
                    match self.request_reader.read_frame_async().await {
                        Ok(frame) => frame,
                        Err(Error::Terminated) => return Err(RpcError::ConnectionClosed),
                        Err(e) => return Err(e.into()),
                    };

                // Skip stale lifecycle frames from previous sessions: genuine
                // requests never carry stream flags.
                let lifecycle = FrameHeader::FLAG_STREAM_CANCEL
                    | FrameHeader::FLAG_STREAM_END
                    | FrameHeader::FLAG_STREAM_ITEM;
                if flags & lifecycle != 0 {
                    continue;
                }

                return Ok(RpcBidiStreamRequest {
                    reply_writer: &mut self.reply_writer,
                    request_reader: &mut self.request_reader,
                    payload_bytes,
                    correlation,
                    _phantom: PhantomData,
                });
            }
        }

        let region_id = self.request_reader.inner().region_id();
        let mut last_generation = self
            .request_reader
            .generation()
            .unwrap_or(0)
            .wrapping_sub(1);

        loop {
            let current_generation = self.request_reader.generation().unwrap_or(0);

            if current_generation != last_generation {
                last_generation = current_generation;

                match self.request_reader.read_frame() {
                    Ok((payload_bytes, correlation, flags)) => {
                        // Skip stale lifecycle frames from previous sessions:
                        // genuine requests never carry stream flags.
                        let lifecycle = FrameHeader::FLAG_STREAM_CANCEL
                            | FrameHeader::FLAG_STREAM_END
                            | FrameHeader::FLAG_STREAM_ITEM;
                        if flags & lifecycle != 0 {
                            continue;
                        }
                        return Ok(RpcBidiStreamRequest {
                            reply_writer: &mut self.reply_writer,
                            request_reader: &mut self.request_reader,
                            payload_bytes,
                            correlation,
                            _phantom: PhantomData,
                        });
                    }
                    Err(Error::BufferEmpty) => {}
                    Err(e) => return Err(e.into()),
                }
            }

            if self.request_reader.poll_peer_closed()? {
                return Err(RpcError::ConnectionClosed);
            }

            if region_id != 0 {
                crate::generation_wait(region_id, last_generation).await;
            }
        }
    }
}

impl<'a, Req, Item, Resp, M: MessageTransport> RpcBidiStreamRequest<'a, Req, Item, Resp, M>
where
    Req: FlatMsg,
    Item: FlatMsg,
    Resp: FlatMsg,
{
    /// Returns a reference to the raw request payload bytes.
    pub fn payload_bytes(&self) -> &[u8] {
        &self.payload_bytes
    }

    /// Decodes the request payload.
    pub fn payload(&self) -> std::result::Result<Req, RpcError> {
        Req::decode(&self.payload_bytes)
            .map_err(|e| RpcError::Serialization(format!("decode request: {e}")))
    }

    /// Decodes and returns the request payload by value.
    pub fn into_payload(self) -> std::result::Result<Req, RpcError> {
        self.payload()
    }

    /// Returns the correlation tag for this session.
    pub fn correlation(&self) -> u32 {
        self.correlation
    }

    /// Splits the bidi session into independent send and receive halves.
    ///
    /// The server can send items through [`BidiResponder`] while
    /// concurrently receiving items from [`BidiRequestStream`]. Each
    /// half closes independently; the server may continue sending after
    /// the client has ended its send direction.
    pub fn split(&mut self) -> (BidiResponder<'_, Resp, M>, BidiRequestStream<'_, Item, M>) {
        (
            BidiResponder {
                writer: self.reply_writer,
                correlation: self.correlation,
                done: false,
                _phantom: PhantomData,
            },
            BidiRequestStream {
                reader: self.request_reader,
                correlation: self.correlation,
                done: false,
                _phantom: PhantomData,
            },
        )
    }
}

impl<Resp, M: MessageTransport> BidiResponder<'_, Resp, M> {
    /// Sends a stream item to the client.
    ///
    /// Parks on a full ring via the generation-wait mechanism (backpressure
    /// honesty).
    pub async fn send(&mut self, item: Resp) -> std::result::Result<(), RpcError>
    where
        Resp: FlatMsg,
    {
        if self.done {
            return Err(RpcError::ConnectionClosed);
        }

        let encoded = FlatMsg::encode(&item);
        let flags = FrameHeader::FLAG_READY | FrameHeader::FLAG_STREAM_ITEM;
        self.writer
            .write_frame_with_flags_async(&encoded, self.correlation, flags)
            .await?;
        Ok(())
    }

    /// Sends the final item and closes the send direction.
    pub async fn close_with_item(&mut self, item: Resp) -> std::result::Result<(), RpcError>
    where
        Resp: FlatMsg,
    {
        if self.done {
            return Err(RpcError::ConnectionClosed);
        }
        self.done = true;

        let encoded = FlatMsg::encode(&item);
        let flags =
            FrameHeader::FLAG_READY | FrameHeader::FLAG_STREAM_ITEM | FrameHeader::FLAG_STREAM_END;
        self.writer
            .write_frame_with_flags_async(&encoded, self.correlation, flags)
            .await?;
        Ok(())
    }

    /// Closes the send direction with no final payload.
    pub async fn close(&mut self) -> std::result::Result<(), RpcError> {
        if self.done {
            return Ok(());
        }
        self.done = true;

        let flags = FrameHeader::FLAG_READY | FrameHeader::FLAG_STREAM_END;
        self.writer
            .write_frame_with_flags_async(&[], self.correlation, flags)
            .await?;
        Ok(())
    }

    /// Cancels this session's reply direction: the client's receive half
    /// observes termination (not a normal end-of-stream), signalling that
    /// the server abandoned the stream rather than completed it.
    ///
    /// Calling `cancel` on an already-closed direction is harmless.
    pub fn cancel(&mut self) {
        if self.done {
            return;
        }
        self.done = true;

        // Best-effort: on a full ring the write is skipped; the client then
        // terminates via peer-close detection instead.
        let flags = FrameHeader::FLAG_READY | FrameHeader::FLAG_STREAM_CANCEL;
        drop(
            self.writer
                .write_frame_with_flags(&[], self.correlation, flags),
        );
    }

    /// Returns `true` if the send direction has been closed.
    pub fn is_closed(&self) -> bool {
        self.done
    }
}

impl<Resp, M: MessageTransport> Drop for BidiResponder<'_, Resp, M> {
    fn drop(&mut self) {
        if !self.done {
            self.done = true;
            let flags = FrameHeader::FLAG_READY | FrameHeader::FLAG_STREAM_END;
            drop(
                self.writer
                    .write_frame_with_flags(&[], self.correlation, flags),
            );
        }
    }
}

impl<Item, M: MessageTransport> BidiRequestStream<'_, Item, M> {
    /// Receives the next item from the client, or `None` if the client
    /// has ended its send direction.
    ///
    /// This is a non-blocking poll — returns `Ok(None)` if no frame is
    /// immediately available.
    pub fn try_recv(&mut self) -> std::result::Result<Option<Item>, RpcError>
    where
        Item: FlatMsg,
    {
        if self.done {
            return Ok(None);
        }

        match try_poll_ready(self.reader) {
            Ok(true) => {}
            Ok(false) => return Ok(None),
            Err(e) => return Err(e.into()),
        }

        match self.reader.read_frame() {
            Ok((payload, tag, flags)) => {
                if tag != self.correlation {
                    // Invariant: one active bidi session per connection pair;
                    // report "no data" rather than misinterpreting the frame.
                    return Ok(None);
                }

                if FrameHeader::FLAG_STREAM_CANCEL & flags != 0 {
                    self.done = true;
                    return Ok(None);
                }

                if FrameHeader::FLAG_STREAM_ERROR & flags != 0 {
                    self.done = true;
                    let message = String::from_utf8_lossy(&payload).into_owned();
                    return Err(RpcError::Remote(message));
                }

                if FrameHeader::FLAG_STREAM_END & flags != 0 {
                    self.done = true;
                }

                if payload.is_empty() && FrameHeader::FLAG_STREAM_END & flags != 0 {
                    return Ok(None);
                }

                FlatMsg::decode(&payload)
                    .map(Some)
                    .map_err(|e| RpcError::Serialization(format!("{e}")))
            }
            Err(Error::BufferEmpty) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Asynchronously receives the next item from the client.
    ///
    /// Parks via generation-wait. Returns `None` when the client has
    /// ended its send direction.
    pub async fn recv(&mut self) -> std::result::Result<Option<Item>, RpcError>
    where
        Item: FlatMsg,
    {
        // Socket transports park on the transport's read waker.
        if self.reader.inner().region_id() == 0 {
            loop {
                if self.done {
                    return Ok(None);
                }

                let (payload, tag, flags) = match self.reader.read_frame_async().await {
                    Ok(frame) => frame,
                    Err(Error::Terminated) => return Err(RpcError::ConnectionClosed),
                    Err(e) => return Err(e.into()),
                };

                if tag != self.correlation {
                    // Invariant: one active bidi session per connection pair;
                    // skip foreign frames.
                    continue;
                }

                if FrameHeader::FLAG_STREAM_CANCEL & flags != 0 {
                    self.done = true;
                    return Ok(None);
                }

                if FrameHeader::FLAG_STREAM_ERROR & flags != 0 {
                    self.done = true;
                    let message = String::from_utf8_lossy(&payload).into_owned();
                    return Err(RpcError::Remote(message));
                }

                let is_end = FrameHeader::FLAG_STREAM_END & flags != 0;
                if is_end {
                    self.done = true;
                }
                if payload.is_empty() && is_end {
                    return Ok(None);
                }

                return FlatMsg::decode(&payload)
                    .map(Some)
                    .map_err(|e| RpcError::Serialization(format!("{e}")));
            }
        }

        let region_id = self.reader.inner().region_id();
        let mut last_generation = self.reader.generation().unwrap_or(0).wrapping_sub(1);

        loop {
            if self.done {
                return Ok(None);
            }

            let current_generation = self.reader.generation().unwrap_or(0);

            if current_generation != last_generation {
                last_generation = current_generation;

                match self.reader.read_frame() {
                    Ok((payload, tag, flags)) => {
                        if tag != self.correlation {
                            // Invariant: one active bidi session per
                            // connection pair; skip foreign frames.
                            continue;
                        }

                        if FrameHeader::FLAG_STREAM_CANCEL & flags != 0 {
                            self.done = true;
                            return Ok(None);
                        }

                        if FrameHeader::FLAG_STREAM_ERROR & flags != 0 {
                            self.done = true;
                            let message = String::from_utf8_lossy(&payload).into_owned();
                            return Err(RpcError::Remote(message));
                        }

                        let is_end = FrameHeader::FLAG_STREAM_END & flags != 0;

                        if is_end {
                            self.done = true;
                        }

                        if payload.is_empty() && is_end {
                            return Ok(None);
                        }

                        return FlatMsg::decode(&payload)
                            .map(Some)
                            .map_err(|e| RpcError::Serialization(format!("{e}")));
                    }
                    Err(Error::BufferEmpty) => {}
                    Err(e) => return Err(e.into()),
                }
            }

            if self.reader.poll_peer_closed()? {
                return Err(RpcError::ConnectionClosed);
            }

            if region_id != 0 {
                crate::generation_wait(region_id, last_generation).await;
            }
        }
    }

    /// Returns `true` if the receive direction has been closed.
    pub fn is_closed(&self) -> bool {
        self.done
    }
}

/// Registers a generation wait on the reader's region for cooperative
/// scheduling, falling back to a waker wake if no callback is installed.
///
/// Regions without an id (e.g. RPC session sub-channels carved from a
/// parent region) cannot name a generation-wait registration, so the task
/// self-wakes to keep polling — the same fallback `generation_wait` applies.
/// Without this, a stream reader parked before its peer's first frame would
/// never be re-polled.
fn register_gen_wait<M: MessageTransport>(reader: &mut FramedRead<M>, cx: &mut Context<'_>) {
    let region_id = reader.inner().region_id();
    if region_id != 0 {
        let cur_gen = reader.generation().unwrap_or(0);
        if !selium_memory::register_generation_wait(region_id, cur_gen, cx.waker()) {
            cx.waker().wake_by_ref();
        }
    } else {
        cx.waker().wake_by_ref();
    }
}

/// Non-panicking wrapper around `FramedRead::poll_ready`.
fn try_poll_ready<M: MessageTransport>(reader: &mut FramedRead<M>) -> WireResult<bool> {
    match reader.poll_ready() {
        Ok(ready) => Ok(ready),
        Err(Error::BufferEmpty) => Ok(false),
        Err(e) => Err(e),
    }
}
