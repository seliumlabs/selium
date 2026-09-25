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

/// A duplex shared-memory transport wrapping a read channel and a write channel.
///
/// The `reader` channel is the direction from which this transport receives
/// frames; the `writer` channel is the direction to which this transport sends
/// frames. Both channels must be initialised and have at least one remote
/// writer/reader respectively.
pub struct ShmTransport {
    reader: Reader,
    writer: BlockingWriter,
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
    /// Creates a new transport from a read channel and a write channel.
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
        let reader = if read_channel.backpressure() == ChannelBackpressure::Park {
            Reader::Blocking(read_channel.blocking_reader()?)
        } else {
            Reader::NonBlocking(read_channel.reader())
        };
        let writer = write_channel.blocking_writer()?;
        Ok(Self {
            reader,
            writer,
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
            reader,
            writer,
            last_generation: 0,
        })
    }

    /// Returns a reference to the underlying writer.
    pub fn writer(&self) -> &BlockingWriter {
        &self.writer
    }

    /// Returns the shared region id of the read channel.
    pub fn read_region_id(&self) -> u64 {
        self.reader.region().region_id()
    }

    /// Returns the shared region id of the write channel.
    pub fn write_region_id(&self) -> u64 {
        self.writer.region().region_id()
    }
}

impl AsyncRead for ShmTransport {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.reader).poll_read(cx, buf)
    }
}

impl AsyncWrite for ShmTransport {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.writer).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.writer).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.writer).poll_shutdown(cx)
    }
}

impl MessageTransport for ShmTransport {
    type Error = std::io::Error;

    fn poll_ready(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<bool>> {
        match self.reader.generation() {
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
        match self.reader.region().load_writer_count() {
            Ok(0) => Poll::Ready(Ok(true)),
            Ok(_) => Poll::Ready(Ok(false)),
            Err(error) => Poll::Ready(Err(error)),
        }
    }

    fn generation(&self) -> Result<u64> {
        self.reader.generation()
    }

    fn region_id(&self) -> u64 {
        self.read_region_id()
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
        let _writer = read_channel.blocking_writer().expect("read channel writer");

        let mut transport = ShmTransport::new(&read_channel, &write_channel).expect("transport");
        assert_eq!(transport.generation().expect("gen"), 0);

        let waker = futures::task::noop_waker();
        let mut cx = std::task::Context::from_waker(&waker);
        match std::pin::Pin::new(&mut transport).poll_peer_closed(&mut cx) {
            std::task::Poll::Ready(Ok(closed)) => assert!(!closed),
            other => panic!("unexpected poll_peer_closed result: {other:?}"),
        }
    }
}
