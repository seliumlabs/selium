//! Transport-agnostic pub/sub handles.

use std::{
    marker::PhantomData,
    pin::Pin,
    task::{Context, Poll},
};

use futures::{Sink, Stream};
use selium_service::FlatMsg;

use crate::{
    MessageTransport,
    error::{Error, Result},
    framed::{FramedRead, FramedWrite},
};

/// A typed publisher that writes encoded messages as frames to a transport.
pub struct Publisher<T, M> {
    writer: FramedWrite<M>,
    writer_id: u32,
    next_mutation_id: u64,
    _t: PhantomData<T>,
}

/// A typed subscriber that reads encoded messages as frames from a transport.
pub struct Subscriber<T, M> {
    reader: FramedRead<M>,
    /// Ring/data capacity used for overwrite detection on transports that
    /// expose a generation counter. `None` disables overwrite detection.
    capacity: Option<u64>,
    last_generation: u64,
    _t: PhantomData<T>,
}

impl<T, M: MessageTransport> Publisher<T, M> {
    /// Creates a new publisher wrapping the given framed writer.
    ///
    /// The publisher is assigned writer id `0` by default. Use
    /// [`set_writer_id`](Self::set_writer_id) to distinguish this publisher
    /// when its tag is read by a subscriber.
    pub fn new(writer: FramedWrite<M>) -> Self {
        Self {
            writer,
            writer_id: 0,
            next_mutation_id: 1,
            _t: PhantomData,
        }
    }

    /// Returns the writer id used as the frame tag for published messages.
    pub fn writer_id(&self) -> u32 {
        self.writer_id
    }

    /// Sets the writer id used as the frame tag for published messages.
    pub fn set_writer_id(&mut self, writer_id: u32) {
        self.writer_id = writer_id;
    }

    /// Allocates a monotonically increasing mutation id for ordered operations
    /// such as live-table writes.
    pub fn allocate_mutation_id(&mut self) -> u64 {
        let id = self.next_mutation_id;
        self.next_mutation_id = self.next_mutation_id.wrapping_add(1);
        id
    }

    /// Returns a reference to the inner framed writer.
    pub fn writer(&self) -> &FramedWrite<M> {
        &self.writer
    }

    /// Returns a mutable reference to the inner framed writer.
    pub fn writer_mut(&mut self) -> &mut FramedWrite<M> {
        &mut self.writer
    }

    /// Consumes this publisher and returns the inner framed writer.
    pub fn into_writer(self) -> FramedWrite<M> {
        self.writer
    }

    /// Publishes a typed message synchronously.
    pub fn publish(&mut self, item: &T) -> Result<()>
    where
        T: FlatMsg,
    {
        let bytes = FlatMsg::encode(item);
        self.writer.write_frame(&bytes, self.writer_id)
    }
}

impl<T: FlatMsg + Unpin, M: MessageTransport> Sink<T> for Publisher<T, M> {
    type Error = Error;

    fn poll_ready(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn start_send(self: Pin<&mut Self>, item: T) -> Result<()> {
        let this = self.get_mut();
        let bytes = FlatMsg::encode(&item);
        this.writer.write_frame(&bytes, this.writer_id)?;
        Ok(())
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_close(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<()>> {
        Poll::Ready(Ok(()))
    }
}

impl<T, M: MessageTransport> Subscriber<T, M> {
    /// Creates a new subscriber wrapping the given framed reader.
    ///
    /// `capacity` is used for generation-based overwrite detection; pass
    /// `None` to disable it.
    pub fn new(reader: FramedRead<M>, capacity: Option<u64>) -> Self {
        let last_generation = reader.generation().unwrap_or(0);
        Self {
            reader,
            capacity,
            last_generation,
            _t: PhantomData,
        }
    }

    /// Returns a reference to the inner framed reader.
    pub fn reader(&self) -> &FramedRead<M> {
        &self.reader
    }

    /// Returns a mutable reference to the inner framed reader.
    pub fn reader_mut(&mut self) -> &mut FramedRead<M> {
        &mut self.reader
    }

    /// Consumes this subscriber and returns the inner framed reader.
    pub fn into_reader(self) -> FramedRead<M> {
        self.reader
    }

    /// Reads the next raw message with its tag.
    ///
    /// Returns `(decoded_message, tag)`. For pub/sub the tag is the writer id.
    pub fn read_with_tag(&mut self) -> Result<(T, u32)>
    where
        T: FlatMsg,
    {
        let (payload, tag, _flags) = self.reader.read_frame()?;
        self.last_generation = self.reader.generation()?;
        let value: T =
            FlatMsg::decode(&payload).map_err(|e| Error::SerializationFailed(format!("{e}")))?;
        Ok((value, tag))
    }

    /// Updates the capacity used for overwrite detection.
    pub fn set_capacity(&mut self, capacity: u64) {
        self.capacity = Some(capacity);
    }
}

impl<T: FlatMsg + Unpin, M: MessageTransport> Stream for Subscriber<T, M> {
    type Item = Result<T>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();

        // Socket transports (no generation counter) park on the transport's
        // `AsyncRead` waker; rings use the generation-based read below.
        if this.reader.inner().region_id() == 0 {
            return match this.reader.poll_frame(cx) {
                Poll::Ready(Ok((payload, _tag, _flags))) => match FlatMsg::decode(&payload) {
                    Ok(value) => Poll::Ready(Some(Ok(value))),
                    Err(e) => Poll::Ready(Some(Err(Error::SerializationFailed(format!("{e}"))))),
                },
                Poll::Ready(Err(Error::Terminated)) => Poll::Ready(None),
                Poll::Ready(Err(e)) => Poll::Ready(Some(Err(e))),
                Poll::Pending => Poll::Pending,
            };
        }

        let has_frame = match this.reader.poll_ready() {
            Ok(ready) => ready,
            Err(Error::BufferEmpty) => false,
            Err(e) => return Poll::Ready(Some(Err(e))),
        };

        if has_frame {
            let current_generation = match this.reader.generation() {
                Ok(g) => g,
                Err(e) => return Poll::Ready(Some(Err(e))),
            };

            // Overwrite detection: if the generation advanced by more than the
            // capacity since the last read, data was overwritten.
            if let Some(capacity) = this.capacity
                && current_generation != 0
                && this.last_generation != 0
            {
                let delta = current_generation.wrapping_sub(this.last_generation);
                if delta > capacity {
                    return Poll::Ready(Some(Err(Error::Overwritten)));
                }
            }

            let (payload, _tag, _flags) = match this.reader.read_frame() {
                Ok(frame) => frame,
                Err(Error::BufferEmpty) => {
                    // Spurious readiness; register wait instead of spinning.
                    let region_id = this.reader.inner().region_id();
                    if region_id != 0 {
                        let cur_gen = this.reader.generation().unwrap_or(0);
                        if !selium_memory::register_generation_wait(region_id, cur_gen, cx.waker())
                        {
                            cx.waker().wake_by_ref();
                        }
                    }
                    return Poll::Pending;
                }
                Err(e) => return Poll::Ready(Some(Err(e))),
            };

            this.last_generation = current_generation;

            match FlatMsg::decode(&payload) {
                Ok(value) => Poll::Ready(Some(Ok(value))),
                Err(e) => Poll::Ready(Some(Err(Error::SerializationFailed(format!("{e}"))))),
            }
        } else {
            // No data available — register a generation wait instead of spinning.
            let region_id = this.reader.inner().region_id();
            if region_id != 0 {
                let cur_gen = this.reader.generation().unwrap_or(0);
                if !selium_memory::register_generation_wait(region_id, cur_gen, cx.waker()) {
                    cx.waker().wake_by_ref();
                }
            }
            Poll::Pending
        }
    }
}
