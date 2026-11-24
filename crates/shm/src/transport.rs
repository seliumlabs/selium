//! Shared-memory transport implementing [`selium_wire::MessageTransport`].

use std::{
    collections::VecDeque,
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll},
};

use selium_wire::{
    MessageTransport,
    error::{Error, Result},
};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

use crate::{
    Channel,
    channels::{BlockingWriter, ChannelBackpressure},
};

/// Type alias matching the OpenSpec task name.
pub type ShmRendezvous = MemoryRendezvous;

/// A shared-memory transport.
///
/// A duplex transport (see [`ShmTransport::new`]) wraps a read channel and a
/// write channel. Point-to-point session users should prefer the directional
/// constructors ([`ShmTransport::read_only`],
/// [`ShmTransport::read_only_awaiting_peer`], and
/// [`ShmTransport::write_only`]): registering only the membership a direction
/// requires keeps the peer's writer count reachable at zero, so withdrawal
/// stays observable. Operations on a direction the transport was not built
/// with return an explicit I/O error instead of silently succeeding.
pub struct ShmTransport {
    reader: Option<Reader>,
    writer: Option<BlockingWriter>,
    last_generation: u64,
}

/// The read side of a [`ShmTransport`].
///
/// Park channels use a blocking reader (slot-protected, enables writer
/// backpressure); Drop channels use a non-blocking reader.
enum Reader {
    NonBlocking(crate::channels::Reader),
    Blocking(crate::channels::BlockingReader),
}

/// In-memory rendezvous for tests, passing region ids between client and server.
#[derive(Clone)]
pub struct MemoryRendezvous {
    inner: Arc<Mutex<VecDeque<u64>>>,
}

impl ShmTransport {
    /// Creates a new duplex transport from a read channel and a write channel.
    ///
    /// On Park channels the read side uses a *blocking* reader that registers
    /// a reader slot in shared memory; without that slot, writers never
    /// observe reader progress and Park backpressure cannot engage. Drop
    /// channels keep the non-blocking reader (jump-to-tail on overtaken).
    ///
    /// # Errors
    ///
    /// Returns an error if the blocking reader or writer cannot be created
    /// (e.g. slot allocation failure).
    pub fn new(read_channel: &Channel, write_channel: &Channel) -> Result<Self> {
        let reader = Self::attached_reader(read_channel)?;
        let writer = write_channel.blocking_writer()?;
        Ok(Self {
            reader: Some(reader),
            writer: Some(writer),
            last_generation: 0,
        })
    }

    /// Creates a transport whose read side replays the ring from position 0.
    ///
    /// Used by live-table consumers that attach after the writer has already
    /// published mutations: they must materialise the full stream, not just
    /// frames written after attachment (cf. [`Self::new`], whose reader starts
    /// at the live tail).
    ///
    /// # Errors
    ///
    /// Returns an error if the blocking reader or writer cannot be created.
    pub fn new_replay(read_channel: &Channel, write_channel: &Channel) -> Result<Self> {
        let reader = Reader::Blocking(read_channel.blocking_reader_from(0)?);
        let writer = write_channel.blocking_writer()?;
        Ok(Self {
            reader: Some(reader),
            writer: Some(writer),
            last_generation: 0,
        })
    }

    /// Creates a read-only transport whose peer is considered already
    /// attached.
    ///
    /// Used by rendezvous recipients (the sender's prior existence is
    /// established by the handoff) and by other consumers that own only a
    /// receive direction. An empty ring with no writers reports EOF.
    pub fn read_only(read_channel: &Channel) -> Result<Self> {
        Ok(Self {
            reader: Some(Self::attached_reader(read_channel)?),
            writer: None,
            last_generation: 0,
        })
    }

    /// Creates a read-only transport that waits for its first peer.
    ///
    /// Initial absence of a point-to-point writer is pending attachment, not
    /// EOF: the reader parks until writer presence, generation movement,
    /// committed data, or explicit departure establishes the peer's existence.
    /// This is the allocator-side read half of per-stream sessions.
    pub fn read_only_awaiting_peer(read_channel: &Channel) -> Result<Self> {
        let reader = if read_channel.backpressure() == ChannelBackpressure::Park {
            Reader::Blocking(read_channel.blocking_reader_awaiting_peer()?)
        } else {
            // Drop channels use the slotless reader; initial absence is not
            // observable there, so the legacy jump-to-tail policy remains.
            Reader::NonBlocking(read_channel.reader())
        };
        Ok(Self {
            reader: Some(reader),
            writer: None,
            last_generation: 0,
        })
    }

    /// Creates a write-only transport.
    ///
    /// Registers only a blocking writer: no reader slot is allocated on the
    /// inbound ring and no self-writer is retained there, so the remote
    /// writer's withdrawal remains visible to its receiver.
    pub fn write_only(write_channel: &Channel) -> Result<Self> {
        Ok(Self {
            reader: None,
            writer: Some(write_channel.blocking_writer()?),
            last_generation: 0,
        })
    }

    /// Builds the read half of a duplex or already-attached transport.
    fn attached_reader(read_channel: &Channel) -> Result<Reader> {
        if read_channel.backpressure() == ChannelBackpressure::Park {
            Ok(Reader::Blocking(read_channel.blocking_reader()?))
        } else {
            Ok(Reader::NonBlocking(read_channel.reader()))
        }
    }

    /// Returns the underlying writer, if this transport has a write direction.
    pub fn writer(&self) -> Option<&BlockingWriter> {
        self.writer.as_ref()
    }

    /// Returns the shared region id of the read channel, or 0 when this
    /// transport has no read direction.
    pub fn read_region_id(&self) -> u64 {
        self.reader
            .as_ref()
            .map_or(0, |reader| reader.region().region_id())
    }

    /// Returns the shared region id of the write channel, or 0 when this
    /// transport has no write direction.
    pub fn write_region_id(&self) -> u64 {
        self.writer
            .as_ref()
            .map_or(0, |writer| writer.region().region_id())
    }
}

impl AsyncRead for ShmTransport {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        match self.reader.as_mut() {
            Some(reader) => Pin::new(reader).poll_read(cx, buf),
            None => Poll::Ready(Err(std::io::Error::other(Error::Transport(
                "transport has no read direction".to_string(),
            )))),
        }
    }
}

impl AsyncWrite for ShmTransport {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        match self.writer.as_mut() {
            Some(writer) => Pin::new(writer).poll_write(cx, buf),
            None => Poll::Ready(Err(std::io::Error::other(Error::Transport(
                "transport has no write direction".to_string(),
            )))),
        }
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match self.writer.as_mut() {
            Some(writer) => Pin::new(writer).poll_flush(cx),
            None => Poll::Ready(Err(std::io::Error::other(Error::Transport(
                "transport has no write direction".to_string(),
            )))),
        }
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match self.writer.as_mut() {
            Some(writer) => Pin::new(writer).poll_shutdown(cx),
            None => Poll::Ready(Err(std::io::Error::other(Error::Transport(
                "transport has no write direction".to_string(),
            )))),
        }
    }
}

impl MessageTransport for ShmTransport {
    type Error = std::io::Error;

    fn poll_ready(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<bool>> {
        let Some(reader) = self.reader.as_ref() else {
            // A write-only transport has no inbound generation to compare
            // against; readiness is always immediately available.
            return Poll::Ready(Ok(true));
        };
        match reader.generation() {
            Ok(generation) => {
                let ready = generation != self.last_generation;
                if ready {
                    self.last_generation = generation;
                }
                Poll::Ready(Ok(ready))
            }
            Err(error) => Poll::Ready(Err(error)),
        }
    }

    fn poll_peer_closed(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<bool>> {
        match self.reader.as_ref() {
            Some(reader) => match reader.peer_closed() {
                Ok(closed) => Poll::Ready(Ok(closed)),
                Err(error) => Poll::Ready(Err(error)),
            },
            // No read direction: no peer to observe closing.
            None => Poll::Ready(Ok(false)),
        }
    }

    fn peer_closed_flag(&self) -> Result<bool> {
        match self.reader.as_ref() {
            Some(reader) => reader.region().peer_closed(),
            None => Ok(false),
        }
    }

    fn generation(&self) -> Result<u64> {
        match self.reader.as_ref() {
            Some(reader) => reader.generation(),
            None => Err(Error::Transport(
                "transport has no read direction".to_string(),
            )),
        }
    }

    fn region_id(&self) -> u64 {
        self.read_region_id()
    }

    fn table_writer_arbiter(&self) -> Option<Arc<dyn selium_wire::TableWriterArbiter>> {
        self.writer.as_ref().map(|writer| {
            Arc::new(writer.region().clone()) as Arc<dyn selium_wire::TableWriterArbiter>
        })
    }
}

impl Reader {
    fn region(&self) -> &crate::ChannelRegion {
        match self {
            Reader::NonBlocking(r) => r.region(),
            Reader::Blocking(r) => r.region(),
        }
    }

    fn generation(&self) -> selium_wire::error::Result<u64> {
        match self {
            Reader::NonBlocking(r) => r.generation(),
            Reader::Blocking(r) => r.generation(),
        }
    }

    /// Reports peer closure using this reader's lifecycle rules.
    fn peer_closed(&self) -> selium_wire::error::Result<bool> {
        match self {
            // The non-blocking reader has no reader-local lifecycle state;
            // zero writers (or an explicit departure mark) is closure.
            Reader::NonBlocking(r) => {
                if r.region().peer_closed()? {
                    Ok(true)
                } else {
                    Ok(r.region().load_writer_count()? == 0)
                }
            }
            Reader::Blocking(r) => r.peer_closed(),
        }
    }
}

impl tokio::io::AsyncRead for Reader {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            Reader::NonBlocking(r) => Pin::new(r).poll_read(cx, buf),
            Reader::Blocking(r) => Pin::new(r).poll_read(cx, buf),
        }
    }
}

impl MemoryRendezvous {
    /// Creates a new empty in-memory rendezvous queue.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(VecDeque::new())),
        }
    }
}

impl selium_wire::Rendezvous for MemoryRendezvous {
    fn send(&self, shared_id: u64) -> impl std::future::Future<Output = Result<()>> + Send {
        let inner = self.inner.clone();
        async move {
            inner
                .lock()
                .map_err(|_e| Error::ChannelClosed)?
                .push_back(shared_id);
            Ok(())
        }
    }

    fn recv(
        &self,
    ) -> impl std::future::Future<Output = Result<selium_wire::rpc::IncomingConnection>> + Send
    {
        let inner = self.inner.clone();
        async move {
            let shared_id = inner
                .lock()
                .map_err(|_e| Error::ChannelClosed)?
                .pop_front()
                .ok_or(Error::BufferEmpty)?;
            Ok(selium_wire::rpc::IncomingConnection {
                client_process_id: 1,
                shared_id,
            })
        }
    }
}

impl Default for MemoryRendezvous {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use selium_abi::ResourceKind;
    use selium_wire::MessageTransport;

    fn setup() {
        crate::install_heap_provider();
    }

    #[tokio::test]
    async fn shm_transport_implements_message_transport() {
        setup();
        let read_channel = Channel::create_with_backpressure(
            1024,
            crate::ChannelBackpressure::Drop,
            ResourceKind::SharedMemory,
        )
        .expect("create read channel");
        let write_channel = Channel::create_with_backpressure(
            1024,
            crate::ChannelBackpressure::Drop,
            ResourceKind::SharedMemory,
        )
        .expect("create write channel");

        // Keep a writer alive on the read channel so writer_count is non-zero.
        // Registration publishes the attachment by advancing the generation.
        let _writer = read_channel.blocking_writer().expect("read channel writer");
        let generation_after_attachment = read_channel
            .ring()
            .region()
            .load_generation()
            .expect("generation");

        let mut transport = ShmTransport::new(&read_channel, &write_channel).expect("transport");
        assert_eq!(
            transport.generation().expect("gen"),
            generation_after_attachment
        );

        let waker = futures::task::noop_waker();
        let mut cx = std::task::Context::from_waker(&waker);
        match std::pin::Pin::new(&mut transport).poll_peer_closed(&mut cx) {
            std::task::Poll::Ready(Ok(closed)) => assert!(!closed),
            other => panic!("unexpected poll_peer_closed result: {other:?}"),
        }
    }
}
