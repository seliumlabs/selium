#![deny(missing_docs)]

use std::{
    future::Future,
    pin::{Pin, pin},
    sync::{Arc, Weak},
    task::{Context, Poll},
};

#[cfg(feature = "loom")]
use loom::sync::atomic::{AtomicBool, AtomicU64, Ordering};
#[cfg(not(feature = "loom"))]
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use pin_project::pin_project;
use selium_kernel::drivers::channel::FrameReadable;
use tokio::io::{AsyncRead, ReadBuf};
use tracing::{Span, debug, instrument};

use crate::{Channel, ChannelError};

/// Convenience type for implementors to treat both reader types as one.
#[pin_project(project = ReaderProj)]
pub enum Reader {
    /// Strong reader variant that maintains backpressure guarantees.
    Strong(#[pin] StrongReader),
    /// Weak reader variant that may drop data when lagging.
    Weak(#[pin] WeakReader),
}

/// Reader that prevents overwriting unread bytes.
pub struct StrongReader {
    /// Ptr to `Channel`
    chan: Weak<Channel>,
    /// Start position being read
    ///
    /// The position is tracked by the `Channel` to prevent overwrites
    pub(crate) pos: Arc<AtomicU64>,
    /// The ID of the position in the `Channel` map
    pos_id: usize,
    /// Termination fuse, which when 'lit' (=true), will safely terminate the reader
    fuse: AtomicBool,
    /// Tracing span for instrumentation
    span: Span,
}

/// Reader that sacrifices retention to avoid blocking writers.
pub struct WeakReader {
    /// Ptr to `Channel`
    chan: Weak<Channel>,
    /// Start position being read
    pub(crate) pos: u64,
    /// Termination fuse, which when 'lit' (=true), will safely terminate the reader
    fuse: AtomicBool,
    /// Tracing span for instrumentation
    span: Span,
}

impl Reader {
    /// Signal termination to the underlying reader variant.
    pub fn terminate(&self) {
        match self {
            Self::Strong(strong) => strong.terminate(),
            Self::Weak(weak) => weak.terminate(),
        }
    }

    /// Extract the strong reader variant, returning `self` on mismatch.
    pub fn into_strong(self) -> std::result::Result<StrongReader, Self> {
        match self {
            Self::Strong(strong) => Ok(strong),
            Self::Weak(_) => Err(self),
        }
    }

    /// Extract the weak reader variant, returning `self` on mismatch.
    pub fn into_weak(self) -> std::result::Result<WeakReader, Self> {
        match self {
            Self::Strong(_) => Err(self),
            Self::Weak(weak) => Ok(weak),
        }
    }
}

impl AsyncRead for Reader {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf,
    ) -> Poll<std::io::Result<()>> {
        match self.project() {
            ReaderProj::Strong(strong) => pin!(strong).poll_read(cx, buf),
            ReaderProj::Weak(weak) => pin!(weak).poll_read(cx, buf),
        }
    }
}

impl From<StrongReader> for Reader {
    fn from(value: StrongReader) -> Self {
        Self::Strong(value)
    }
}

impl From<WeakReader> for Reader {
    fn from(value: WeakReader) -> Self {
        Self::Weak(value)
    }
}

impl StrongReader {
    #[instrument(name = "StrongReader", parent = &chan.span, skip_all, fields(position_id=pos_id))]
    pub(crate) fn new(chan: Arc<Channel>, pos: Arc<AtomicU64>, pos_id: usize) -> Self {
        debug!("create reader");

        Self {
            chan: Arc::downgrade(&chan),
            pos,
            pos_id,
            fuse: AtomicBool::new(false),
            span: Span::current(),
        }
    }

    /// Safely terminate this reader.
    ///
    /// This will cause `poll_read` to error with [std::io::ErrorKind::ConnectionAborted].
    #[instrument(parent = &self.span, skip(self))]
    pub fn terminate(&self) {
        if let Some(chan) = self.chan.upgrade() {
            debug!("terminate reader");

            self.fuse.store(true, Ordering::Release);
            chan.remove_head(self.pos_id);
        }
    }

    /// Read a complete frame, returning the writer identifier and payload bytes.
    pub async fn read_frame(&mut self, max_len: usize) -> std::io::Result<(u16, Vec<u8>)> {
        futures::future::poll_fn(|cx| self.poll_read_frame(cx, max_len)).await
    }

    #[instrument(parent = &self.span, skip_all)]
    fn poll_read_frame(
        &mut self,
        cx: &mut Context<'_>,
        max_len: usize,
    ) -> Poll<std::io::Result<(u16, Vec<u8>)>> {
        let Some(chan) = self.chan.upgrade() else {
            return Poll::Ready(Err(std::io::Error::from(std::io::ErrorKind::BrokenPipe)));
        };

        if self.fuse.load(Ordering::Acquire) || chan.terminated.load(Ordering::Acquire) {
            return Poll::Ready(Err(std::io::Error::from(
                std::io::ErrorKind::ConnectionAborted,
            )));
        }

        let pos = self.pos.load(Ordering::Acquire);

        let draining = chan.draining.load(Ordering::Acquire);

        let Some(frame) = chan.frame_for(pos) else {
            if draining {
                return Poll::Ready(Err(std::io::Error::from(std::io::ErrorKind::Interrupted)));
            }
            chan.enqueue(pos, cx.waker().to_owned());
            debug!("frame metadata pending");
            return Poll::Pending;
        };

        if frame.len as usize > max_len {
            return Poll::Ready(Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "frame exceeds requested length",
            )));
        }

        let end = frame.start + frame.len;
        if chan.get_tail() < end {
            chan.enqueue(end, cx.waker().to_owned());
            debug!("frame pending");
            return Poll::Pending;
        }

        let mut payload = vec![0u8; frame.len as usize];
        if frame.len > 0 {
            unsafe { chan.read_unsafe(pos, &mut payload) };
        }

        self.pos.store(end, Ordering::Release);
        chan.prune_frames();
        debug!(len = payload.len(), "consumed frame");
        chan.schedule_writers();

        Poll::Ready(Ok((frame.writer_id, payload)))
    }

    /// Convert this strong reader into a weak reader that relinquishes its head slot when idle.
    #[instrument(parent = &self.span, skip(self))]
    pub fn downgrade(self) -> WeakReader {
        debug!("downgrade this reader");

        if let Some(chan) = self.chan.upgrade() {
            chan.remove_head(self.pos_id);
        }

        WeakReader::new_with_state(
            self.chan.clone(),
            self.pos.load(Ordering::Acquire),
            self.fuse.load(Ordering::Acquire),
        )
    }
}

impl FrameReadable for StrongReader {
    fn read_frame(
        &mut self,
        max_len: usize,
    ) -> Pin<Box<dyn Future<Output = std::io::Result<(u16, Vec<u8>)>> + Send + '_>> {
        Box::pin(StrongReader::read_frame(self, max_len))
    }
}

impl Drop for StrongReader {
    fn drop(&mut self) {
        if let Some(chan) = self.chan.upgrade() {
            chan.remove_head(self.pos_id);
        }
    }
}

impl AsyncRead for StrongReader {
    #[instrument(parent = &self.span, skip_all)]
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf,
    ) -> Poll<std::io::Result<()>> {
        let Some(chan) = self.chan.upgrade() else {
            return Poll::Ready(Err(std::io::Error::from(std::io::ErrorKind::BrokenPipe)));
        };

        if self.fuse.load(Ordering::Acquire) || chan.terminated.load(Ordering::Acquire) {
            return Poll::Ready(Err(std::io::Error::from(
                std::io::ErrorKind::ConnectionAborted,
            )));
        }

        let pos = self.pos.load(Ordering::Acquire);

        // If the channel is draining, only allow unfinished reads to proceed
        if chan.draining.load(Ordering::Acquire) {
            return Poll::Ready(Err(std::io::Error::from(std::io::ErrorKind::Interrupted)));
        }

        let filled = buf.filled().len();
        let read = unsafe { chan.read_unsafe(pos, &mut buf.initialized_mut()[filled..]) };
        buf.advance(read);

        if read == 0 {
            chan.enqueue(pos, cx.waker().to_owned());
            debug!("pending");
            Poll::Pending
        } else {
            self.pos.store(pos + read as u64, Ordering::Release);
            debug!(size = read, "consumed bytes");
            chan.schedule_writers();
            Poll::Ready(Ok(()))
        }
    }
}

impl WeakReader {
    #[instrument(name = "WeakReader", parent = &chan.span, skip_all)]
    pub(crate) fn new(chan: Arc<Channel>, pos: u64) -> Self {
        debug!("create reader");

        Self {
            chan: Arc::downgrade(&chan),
            pos,
            fuse: AtomicBool::new(false),
            span: Span::current(),
        }
    }

    #[instrument(name = "WeakReader", parent = &chan.upgrade().expect("channel missing").span, skip_all)]
    fn new_with_state(chan: Weak<Channel>, pos: u64, fuse_state: bool) -> Self {
        let reader = Self {
            chan,
            pos,
            fuse: AtomicBool::new(fuse_state),
            span: Span::current(),
        };
        if fuse_state {
            reader.terminate();
        }
        reader
    }

    /// Safely terminate this reader.
    ///
    /// This will cause `poll_read` to error with [std::io::ErrorKind::ConnectionAborted].
    #[instrument(parent = &self.span, skip(self))]
    pub fn terminate(&self) {
        debug!("terminate");

        self.fuse.store(true, Ordering::Release);
    }

    /// Read a complete frame, returning the writer identifier and payload bytes.
    pub async fn read_frame(&mut self, max_len: usize) -> std::io::Result<(u16, Vec<u8>)> {
        futures::future::poll_fn(|cx| self.poll_read_frame(cx, max_len)).await
    }

    #[instrument(parent = &self.span, skip_all)]
    fn poll_read_frame(
        &mut self,
        cx: &mut Context<'_>,
        max_len: usize,
    ) -> Poll<std::io::Result<(u16, Vec<u8>)>> {
        let Some(chan) = self.chan.upgrade() else {
            return Poll::Ready(Err(std::io::Error::from(std::io::ErrorKind::BrokenPipe)));
        };

        if self.fuse.load(Ordering::Acquire) || chan.terminated.load(Ordering::Acquire) {
            return Poll::Ready(Err(std::io::Error::from(
                std::io::ErrorKind::ConnectionAborted,
            )));
        }

        let draining = chan.draining.load(Ordering::Acquire);

        if let Err(ChannelError::ReaderBehind(pos)) = chan.read(self.pos, &mut []) {
            self.pos = pos;
            return Poll::Ready(Err(std::io::Error::other(ChannelError::ReaderBehind(pos))));
        }

        let Some(frame) = chan.frame_for(self.pos) else {
            if draining {
                return Poll::Ready(Err(std::io::Error::from(std::io::ErrorKind::Interrupted)));
            }
            chan.enqueue(self.pos, cx.waker().to_owned());
            debug!("frame metadata pending");
            return Poll::Pending;
        };

        if frame.len as usize > max_len {
            return Poll::Ready(Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "frame exceeds requested length",
            )));
        }

        let end = frame.start + frame.len;
        if chan.get_tail() < end {
            chan.enqueue(end, cx.waker().to_owned());
            debug!("weak reader frame pending");
            return Poll::Pending;
        }

        let mut payload = vec![0u8; frame.len as usize];
        match chan.read(self.pos, &mut payload) {
            Ok(read) => {
                self.pos = end;
                chan.prune_frames();
                debug!(len = payload.len(), read, "weak reader consumed frame");
                chan.schedule_writers();
                Poll::Ready(Ok((frame.writer_id, payload)))
            }
            Err(ChannelError::ReaderBehind(pos)) => {
                self.pos = pos;
                Poll::Ready(Err(std::io::Error::other(ChannelError::ReaderBehind(pos))))
            }
            Err(_) => unreachable!(),
        }
    }
}

impl FrameReadable for WeakReader {
    fn read_frame(
        &mut self,
        max_len: usize,
    ) -> Pin<Box<dyn Future<Output = std::io::Result<(u16, Vec<u8>)>> + Send + '_>> {
        Box::pin(WeakReader::read_frame(self, max_len))
    }
}

impl AsyncRead for WeakReader {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf,
    ) -> Poll<std::io::Result<()>> {
        let Some(chan) = self.chan.upgrade() else {
            return Poll::Ready(Err(std::io::Error::from(std::io::ErrorKind::BrokenPipe)));
        };

        if self.fuse.load(Ordering::Acquire) || chan.terminated.load(Ordering::Acquire) {
            return Poll::Ready(Err(std::io::Error::from(
                std::io::ErrorKind::ConnectionAborted,
            )));
        }

        if chan.draining.load(Ordering::Acquire) {
            return Poll::Ready(Err(std::io::Error::from(std::io::ErrorKind::Interrupted)));
        }

        let filled = buf.filled().len();
        match chan.read(self.pos, &mut buf.initialized_mut()[filled..]) {
            Ok(read) if read > 0 => {
                self.pos += read as u64;
                buf.advance(read);
                Poll::Ready(Ok(()))
            }
            Ok(_) => {
                chan.enqueue(self.pos, cx.waker().to_owned());
                Poll::Pending
            }
            Err(ChannelError::ReaderBehind(pos)) => {
                self.pos = pos;
                Poll::Ready(Err(std::io::Error::other(ChannelError::ReaderBehind(pos))))
            }
            Err(_) => unreachable!(),
        }
    }
}
