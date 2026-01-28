#![deny(missing_docs)]
//! In-memory asynchronous channel implementation with configurable backpressure.

#[cfg(not(feature = "loom"))]
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use std::{
    pin::Pin,
    sync::{Arc, Weak},
    task::{Context, Poll},
};

#[cfg(feature = "loom")]
use loom::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use pin_project::{pin_project, pinned_drop};
use tokio::io::AsyncWrite;
use tracing::{Span, debug, instrument};

use crate::{Backpressure, Channel, id_factory::Id};

/// Writable handle into a [`Channel`].
#[pin_project(project = WriterProj)]
pub enum Writer {
    /// Strong writer variant that participates in backpressure accounting.
    Strong(#[pin] StrongWriter),
    /// Weak writer variant that does not hold a persistent tail slot.
    Weak(#[pin] WeakWriter),
}

/// Writer that maintains a persistent tail slot and participates in backpressure accounting.
pub struct StrongWriter {
    /// Identifier for writer attribution
    id: Id,
    /// Ptr to `Channel`
    chan: Weak<Channel>,
    /// Start position being written
    ///
    /// The position is tracked by the `Channel` to trigger reads
    pub(crate) pos: Arc<AtomicU64>,
    /// The ID of the position in the `Channel` map
    pos_id: Option<usize>,
    /// Number of bytes left to write
    pub(crate) rem: usize,
    /// Termination fuse, which when 'lit' (=true), will safely terminate the writer
    fuse: AtomicBool,
    /// Tracing span for instrumentation
    span: Span,
}

/// Writer that relinquishes its tail slot when idle.
#[pin_project(PinnedDrop)]
pub struct WeakWriter {
    /// Identifier for writer attribution
    id: Id,
    /// Ptr to `Channel`
    chan: Weak<Channel>,
    /// Termination fuse, which when 'lit' (=true), will safely terminate the writer
    fuse: AtomicBool,
    /// Writes are channeled through a temporary `StrongWriter` which is dropped after
    /// writing a full frame.
    #[pin]
    current: Option<StrongWriter>,
    /// Tracing span for instrumentation
    span: Span,
}

impl StrongWriter {
    #[instrument(name = "StrongWriter", parent = &chan.span, skip_all, fields(id = id.get()))]
    pub(crate) fn new(
        id: Id,
        chan: Arc<Channel>,
        pos: Arc<AtomicU64>,
        pos_id: Option<usize>,
    ) -> Self {
        Self {
            id,
            chan: Arc::downgrade(&chan),
            pos,
            pos_id,
            rem: 0,
            fuse: AtomicBool::new(false),
            span: Span::current(),
        }
    }

    fn release_tail(&mut self) {
        if let Some(id) = self.pos_id.take()
            && let Some(chan) = self.chan.upgrade()
        {
            chan.remove_tail(id);
        }
    }

    fn is_idle(&self) -> bool {
        self.rem == 0
    }

    /// Current number of bytes the writer can append without blocking.
    pub fn writable_size(&self) -> u64 {
        if let Some(chan) = self.chan.upgrade() {
            chan.writable_size(self.pos.load(Ordering::Acquire))
        } else {
            0
        }
    }

    /// Safely terminate this writer.
    ///
    /// This will cause `poll_write` to error with [std::io::ErrorKind::ConnectionAborted].
    #[instrument(parent = &self.span, skip(self))]
    pub fn terminate(&mut self) {
        debug!("terminate");

        self.fuse.store(true, Ordering::Release);
        self.release_tail();
    }

    /// Convert this strong writer into a weak writer that relinquishes its tail slot when idle.
    #[instrument(parent = &self.span, skip(self))]
    pub fn downgrade(mut self) -> WeakWriter {
        debug!("downgrade this writer");

        self.release_tail();

        WeakWriter::new_with_state(
            self.id.clone(),
            self.chan.clone(),
            self.fuse.load(Ordering::Acquire),
            None,
        )
    }
}

impl Drop for StrongWriter {
    fn drop(&mut self) {
        self.release_tail();
    }
}

impl AsyncWrite for StrongWriter {
    #[instrument(parent = &self.span, skip_all)]
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let Some(chan) = self.chan.upgrade() else {
            return Poll::Ready(Err(std::io::Error::from(std::io::ErrorKind::BrokenPipe)));
        };

        // If the channel or writer is terminating, abort the write
        if self.fuse.load(Ordering::Acquire) || chan.terminated.load(Ordering::Acquire) {
            return Poll::Ready(Err(std::io::Error::from(
                std::io::ErrorKind::ConnectionAborted,
            )));
        }

        // If the channel is draining, only allow unfinished writes to proceed
        if chan.draining.load(Ordering::Acquire) && self.rem == 0 {
            return Poll::Ready(Err(std::io::Error::from(std::io::ErrorKind::Interrupted)));
        }

        let len = buf.len();

        // If no bytes remain in the current slice, reserve a new slice
        let pos = if self.rem == 0 {
            // In Drop mode, if no space is available, drop immediately without reserving
            if matches!(chan.backpressure, Backpressure::Drop) {
                let avail = chan.writable_size(self.pos.load(Ordering::Acquire)) as usize;
                if avail < len {
                    return Poll::Ready(Ok(0));
                }
                let reserve = len as u64;
                let pos = chan.reserve_slice(reserve);
                self.pos.store(pos, Ordering::Release);
                self.rem = len;
                chan.register_frame(pos, reserve, self.id.get());
                pos
            } else {
                let pos = chan.reserve_slice(len as u64);
                self.pos.store(pos, Ordering::Release);
                self.rem = len;
                chan.register_frame(pos, len as u64, self.id.get());
                pos
            }
        } else {
            self.pos.load(Ordering::Acquire)
        };

        let written = chan.write(pos, &buf[..self.rem]);

        if written == 0 {
            debug!("writer poll_write made no progress");
            if matches!(chan.backpressure, Backpressure::Drop) {
                // Do not enqueue; signal that nothing was written.
                return Poll::Ready(Ok(0));
            }
            chan.enqueue(pos, cx.waker().to_owned());
            Poll::Pending
        } else {
            self.pos.store(pos + written as u64, Ordering::Release);
            self.rem -= written;
            debug!(
                pos = self.pos.load(Ordering::Acquire),
                rem = self.rem,
                written,
                "writer poll_write committed bytes"
            );
            chan.schedule_readers();
            Poll::Ready(Ok(written))
        }
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

impl WeakWriter {
    #[instrument(name = "WeakWriter", parent = &chan.span, skip_all, fields(id = id.get()))]
    pub(crate) fn new(id: Id, chan: Arc<Channel>) -> Self {
        Self {
            id,
            chan: Arc::downgrade(&chan),
            fuse: AtomicBool::new(false),
            current: None,
            span: Span::current(),
        }
    }

    #[instrument(name = "WeakWriter", parent = &chan.upgrade().expect("channel missing").span, skip_all, fields(id = id.get()))]
    fn new_with_state(
        id: Id,
        chan: Weak<Channel>,
        fuse_state: bool,
        current: Option<StrongWriter>,
    ) -> Self {
        let mut writer = Self {
            id,
            chan,
            fuse: AtomicBool::new(fuse_state),
            current,
            span: Span::current(),
        };
        if fuse_state {
            writer.terminate();
        }
        writer
    }

    fn ensure_strong(self: Pin<&mut Self>) -> std::io::Result<Pin<&mut StrongWriter>> {
        let Some(chan) = self.chan.upgrade() else {
            return Err(std::io::Error::from(std::io::ErrorKind::BrokenPipe));
        };

        let mut this = self.project();
        if this.fuse.load(Ordering::Acquire) {
            return Err(std::io::Error::from(std::io::ErrorKind::ConnectionAborted));
        }
        if chan.draining.load(Ordering::Acquire) {
            return Err(std::io::Error::from(std::io::ErrorKind::Interrupted));
        }
        if this.current.is_none() {
            let strong = chan.new_strong_writer_with_id(this.id.clone());
            this.current.set(Some(strong));
        }
        Ok(this.current.as_pin_mut().expect("strong writer present"))
    }

    fn release_if_idle(self: Pin<&mut Self>) {
        let mut this = self.project();
        if let Some(mut strong) = this.current.as_mut().as_pin_mut()
            && strong.is_idle()
        {
            strong.as_mut().get_mut().release_tail();
            if let Some(chan) = this.chan.upgrade() {
                chan.schedule_readers();
            }
            this.current.set(None);
        }
    }

    #[instrument(parent = &self.span, skip(self))]
    fn terminate(&mut self) {
        debug!("terminate");

        self.fuse.store(true, Ordering::Release);
        if let Some(mut strong) = self.current.take() {
            strong.terminate();
        }
    }
}

impl AsyncWrite for WeakWriter {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let mut this = self;
        match this.as_mut().ensure_strong() {
            Ok(mut strong) => {
                let result = strong.as_mut().poll_write(cx, buf);
                if matches!(result, Poll::Ready(Ok(_))) {
                    this.release_if_idle();
                }
                result
            }
            Err(err) => Poll::Ready(Err(err)),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        let mut this = self;
        match this.as_mut().ensure_strong() {
            Ok(mut strong) => {
                let result = strong.as_mut().poll_flush(cx);
                this.release_if_idle();
                result
            }
            Err(err) => Poll::Ready(Err(err)),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        let mut this = self;
        match this.as_mut().ensure_strong() {
            Ok(mut strong) => {
                let result = strong.as_mut().poll_shutdown(cx);
                this.release_if_idle();
                result
            }
            Err(err) => Poll::Ready(Err(err)),
        }
    }
}

#[pinned_drop]
impl PinnedDrop for WeakWriter {
    fn drop(self: Pin<&mut Self>) {
        let mut this = self.project();
        if let Some(mut strong) = this.current.take() {
            strong.release_tail();
            if let Some(chan) = strong.chan.upgrade() {
                chan.schedule_readers();
            }
        }
    }
}

impl Writer {
    /// Signal termination to the underlying writer variant.
    pub fn terminate(&mut self) {
        match self {
            Writer::Strong(writer) => writer.terminate(),
            Writer::Weak(writer) => writer.terminate(),
        }
    }

    /// Downgrade the writer to its weak form.
    pub fn downgrade(self) -> Writer {
        match self {
            Writer::Strong(writer) => Writer::Weak(writer.downgrade()),
            Writer::Weak(writer) => Writer::Weak(writer),
        }
    }

    /// Extract the strong writer variant, returning `self` on mismatch.
    #[allow(clippy::result_large_err)]
    pub fn into_strong(self) -> std::result::Result<StrongWriter, Self> {
        match self {
            Writer::Strong(strong) => Ok(strong),
            Writer::Weak(_) => Err(self),
        }
    }

    /// Extract the weak writer variant, returning a downgraded strong writer when required.
    #[allow(clippy::result_large_err)]
    pub fn into_weak(self) -> std::result::Result<WeakWriter, Self> {
        match self {
            Writer::Weak(weak) => Ok(weak),
            Writer::Strong(strong) => Ok(strong.downgrade()),
        }
    }
}

impl AsyncWrite for Writer {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        match self.project() {
            WriterProj::Strong(strong) => strong.poll_write(cx, buf),
            WriterProj::Weak(weak) => weak.poll_write(cx, buf),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match self.project() {
            WriterProj::Strong(strong) => strong.poll_flush(cx),
            WriterProj::Weak(weak) => weak.poll_flush(cx),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match self.project() {
            WriterProj::Strong(strong) => strong.poll_shutdown(cx),
            WriterProj::Weak(weak) => weak.poll_shutdown(cx),
        }
    }
}
