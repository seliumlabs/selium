//! High-level guest IO helpers for creating channels and moving raw frames.
//!
//! # Examples
//! ```no_run
//! use futures::{SinkExt, StreamExt};
//! use selium_userland::{entrypoint, io::{Channel, DriverError}};
//!
//! #[entrypoint]
//! async fn my_service() -> Result<(), DriverError> {
//!     let channel = Channel::create(64 * 1024).await?; // 64kb buffer
//!
//!     let mut writer = channel.publish().await?;
//!     writer.send(b"hello".to_vec()).await?;
//!
//!     let mut reader = channel.subscribe(64 * 1024).await?; // 64kb buffer
//!     if let Some(frame) = reader.next().await.transpose()? {
//!         eprintln!("got {} bytes", frame.payload.len());
//!     }
//!
//!     Ok(())
//! }
//! ```

use core::{
    convert::TryFrom,
    fmt,
    pin::Pin,
    task::{Context, Poll},
};

use futures::{Sink, Stream};
use selium_abi::{ChannelCreate, GuestResourceId, GuestUint, IoFrame, IoRead, IoWrite};

use crate::FromHandle;
pub use crate::driver::{
    DriverError, DriverFuture, DriverModule, MIN_RESULT_CAPACITY, RKYV_VEC_OVERHEAD, RkyvDecoder,
    encode_args,
};
/// Backpressure behaviour for channel writers.
pub use selium_abi::ChannelBackpressure;

const DEFAULT_CHUNK_SIZE: u32 = 64 * 1024; // 64kb

/// Guest-local handle returned by the channel drivers.
pub type ChannelHandle = GuestResourceId;
/// Host registry handle used when sharing a channel between processes.
pub type SharedChannelHandle = GuestResourceId;

/// Handle to a Selium channel that can spawn publishers and subscribers.
#[derive(Clone)]
pub struct Channel(ChannelHandle);

/// Shared reference to a channel resource held in the host registry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SharedChannel(pub GuestResourceId);

/// Convenience wrapper holding a request/response channel pair.
pub struct ChannelPair {
    request: Channel,
    response: Channel,
}

/// Stream wrapper that yields attributed typed frames read from the channel.
pub struct Reader {
    handle: ChannelHandle,
    chunk_size: usize,
    kind: ReaderKind,
    inflight: Option<ReaderInflight>,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum ReaderKind {
    Strong,
    Weak,
}

enum ReaderInflight {
    Strong(DriverFuture<channel_strong_read_frame::Module, RkyvDecoder<IoFrame>>),
    Weak(DriverFuture<channel_weak_read_frame::Module, RkyvDecoder<IoFrame>>),
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum WriterKind {
    Strong,
    Weak,
}

enum WriterInflight {
    Strong(DriverFuture<channel_strong_write_frame::Module, RkyvDecoder<GuestUint>>),
    Weak(DriverFuture<channel_weak_write_frame::Module, RkyvDecoder<GuestUint>>),
}

/// Writer that serialises typed payloads onto the channel.
pub struct Writer {
    handle: ChannelHandle,
    kind: WriterKind,
    inflight: Option<WriterInflight>,
}

impl Channel {
    /// Create a new channel with the requested capacity (in bytes).
    ///
    /// This uses [`ChannelBackpressure::Park`] for writer backpressure.
    ///
    /// The channel is powered by the `channel_create` driver and errors are surfaced as
    /// [`DriverError`] values rather than panicking.
    pub async fn create(capacity: GuestUint) -> Result<Self, DriverError> {
        Self::create_with_backpressure(capacity, ChannelBackpressure::Park).await
    }

    /// Create a new channel with the requested capacity (in bytes) and backpressure behaviour.
    ///
    /// The channel is powered by the `channel_create` driver and errors are surfaced as
    /// [`DriverError`] values rather than panicking.
    pub async fn create_with_backpressure(
        capacity: GuestUint,
        backpressure: ChannelBackpressure,
    ) -> Result<Self, DriverError> {
        let args = encode_args(&ChannelCreate {
            capacity,
            backpressure,
        })?;
        let handle = DriverFuture::<channel_create::Module, RkyvDecoder<GuestUint>>::new(
            &args,
            8,
            RkyvDecoder::new(),
        )?
        .await?;
        // Safe because the handle is minted by the host kernel.
        Ok(unsafe { Self::from_raw(GuestResourceId::from(handle)) })
    }

    /// Delete this channel.
    pub async fn delete(self) -> Result<(), DriverError> {
        let handle = guest_handle(self.0)?;
        let args = encode_args(&handle)?;
        DriverFuture::<channel_delete::Module, RkyvDecoder<()>>::new(&args, 0, RkyvDecoder::new())?
            .await?;
        Ok(())
    }

    /// Drain this channel of data so that it can be removed without data loss.
    pub async fn drain(&self) -> Result<(), DriverError> {
        let handle = guest_handle(self.0)?;
        let args = encode_args(&handle)?;
        DriverFuture::<channel_drain::Module, RkyvDecoder<()>>::new(&args, 0, RkyvDecoder::new())?
            .await?;
        Ok(())
    }

    /// Create a `Channel` from an existing handle.
    ///
    /// # Safety
    /// The handle must have been minted for this guest by the Selium host kernel. Supplying a
    /// forged or stale handle may be rejected by the host or lead to undefined behaviour.
    pub unsafe fn from_raw(handle: ChannelHandle) -> Self {
        Self(handle)
    }

    /// Backwards-compatible alias for [`Channel::from_raw`].
    ///
    /// # Safety
    /// The handle must have been minted for this guest by the Selium host kernel. Supplying a
    /// forged or stale handle may be rejected by the host or lead to undefined behaviour.
    pub unsafe fn new(handle: ChannelHandle) -> Self {
        unsafe { Self::from_raw(handle) }
    }

    /// Expose the underlying handle so it can be gifted to another guest.
    pub fn handle(&self) -> ChannelHandle {
        self.0
    }

    /// Export this channel as a shared reference that another guest can attach to.
    pub async fn share(&self) -> Result<SharedChannel, DriverError> {
        let handle = guest_handle(self.0)?;
        let args = encode_args(&handle)?;
        let handle = DriverFuture::<channel_share::Module, RkyvDecoder<SharedChannelHandle>>::new(
            &args,
            8,
            RkyvDecoder::new(),
        )?
        .await?;
        // Safe because the handle is minted by the host kernel.
        Ok(unsafe { SharedChannel::from_raw(handle) })
    }

    /// Attach to a shared channel reference produced by [`Channel::share`] or [`Channel::detach`].
    pub async fn attach_shared(reference: SharedChannel) -> Result<Self, DriverError> {
        let args = encode_args(&reference.raw())?;
        let handle = DriverFuture::<channel_attach::Module, RkyvDecoder<GuestUint>>::new(
            &args,
            8,
            RkyvDecoder::new(),
        )?
        .await?;
        // Safe because the handle is returned by the host kernel.
        Ok(unsafe { Self::from_raw(GuestResourceId::from(handle)) })
    }

    /// Detach this channel handle from the current guest.
    pub async fn detach(self) -> Result<(), DriverError> {
        let handle = guest_handle(self.0)?;
        let args = encode_args(&handle)?;
        DriverFuture::<channel_detach::Module, RkyvDecoder<()>>::new(&args, 0, RkyvDecoder::new())?
            .await?;
        Ok(())
    }

    /// Create a new channel reader that implements [`Stream`].
    pub async fn subscribe(&self, chunk_size: GuestUint) -> Result<Reader, DriverError> {
        Reader::attach(self.0, chunk_size as usize).await
    }

    /// Create a weak channel reader that implements [`Stream`].
    pub async fn subscribe_weak(&self, chunk_size: GuestUint) -> Result<Reader, DriverError> {
        Reader::attach_weak(self.0, chunk_size as usize).await
    }

    /// Create a new channel writer that implements [`Sink`].
    pub async fn publish(&self) -> Result<Writer, DriverError> {
        Writer::attach(self.0).await
    }

    /// Create a weak channel writer that relinquishes its tail slot when idle.
    pub async fn publish_weak(&self) -> Result<Writer, DriverError> {
        Writer::attach_weak(self.0).await
    }
}

impl SharedChannel {
    /// Return the raw shared handle, suitable for serialization.
    pub fn raw(&self) -> GuestResourceId {
        self.0
    }

    /// Construct a shared channel reference from a raw handle provided by another guest.
    ///
    /// # Safety
    /// The handle must originate from Selium's registry for the current guest. Forged handles will
    /// be rejected by the host or may corrupt channel state.
    pub unsafe fn from_raw(handle: impl Into<GuestResourceId>) -> Self {
        Self(handle.into())
    }

    /// Backwards-compatible alias for [`SharedChannel::from_raw`].
    ///
    /// # Safety
    /// The handle must originate from Selium's registry for the current guest. Forged handles will
    /// be rejected by the host or may corrupt channel state.
    pub unsafe fn new(handle: impl Into<GuestResourceId>) -> Self {
        unsafe { Self::from_raw(handle) }
    }
}

impl FromHandle for SharedChannel {
    type Handles = GuestResourceId;

    unsafe fn from_handle(handle: Self::Handles) -> Self {
        Self(handle)
    }
}

impl fmt::Debug for Channel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Channel").field(&self.0).finish()
    }
}

impl ChannelPair {
    /// Construct a new request/response pair.
    pub fn new(request: Channel, response: Channel) -> Self {
        Self { request, response }
    }

    /// Return the request channel.
    pub fn request(self) -> Channel {
        self.request
    }

    /// Return the response channel.
    pub fn response(self) -> Channel {
        self.response
    }
}

impl Reader {
    /// Override the chunk size used when streaming frames from the channel.
    ///
    /// Smaller chunks reduce buffering at the cost of more driver invocations; larger chunks
    /// amortise driver overhead but increase latency.
    pub fn with_chunk_size(mut self, chunk: usize) -> Self {
        self.chunk_size = chunk.max(1);
        self
    }

    async fn attach(channel: ChannelHandle, chunk_size: usize) -> Result<Self, DriverError> {
        let channel = guest_handle(channel)?;
        let args = encode_args(&channel)?;
        let handle = DriverFuture::<reader_create::Module, RkyvDecoder<GuestUint>>::new(
            &args,
            8,
            RkyvDecoder::new(),
        )?
        .await?;

        Ok(Self {
            handle: GuestResourceId::from(handle),
            chunk_size,
            kind: ReaderKind::Strong,
            inflight: None,
        })
    }

    async fn attach_weak(channel: ChannelHandle, chunk_size: usize) -> Result<Self, DriverError> {
        let channel = guest_handle(channel)?;
        let args = encode_args(&channel)?;
        let handle = DriverFuture::<weak_reader_create::Module, RkyvDecoder<GuestUint>>::new(
            &args,
            8,
            RkyvDecoder::new(),
        )?
        .await?;

        Ok(Self {
            handle: GuestResourceId::from(handle),
            chunk_size,
            kind: ReaderKind::Weak,
            inflight: None,
        })
    }
}

impl ReaderInflight {
    fn poll(&mut self, cx: &mut Context<'_>) -> Poll<Result<IoFrame, DriverError>> {
        match self {
            ReaderInflight::Strong(fut) => Pin::new(fut).poll(cx),
            ReaderInflight::Weak(fut) => Pin::new(fut).poll(cx),
        }
    }
}

impl Stream for Reader {
    type Item = Result<IoFrame, DriverError>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let mut this = self.as_mut();

        if this.chunk_size == 0 {
            return Poll::Ready(Some(Err(DriverError::InvalidArgument)));
        }

        if this.inflight.is_none() {
            let len32 = match u32::try_from(this.chunk_size) {
                Ok(v) => v,
                Err(_) => return Poll::Ready(Some(Err(DriverError::InvalidArgument))),
            };
            let handle = match guest_handle(this.handle) {
                Ok(handle) => handle,
                Err(err) => return Poll::Ready(Some(Err(err))),
            };
            let args = IoRead { handle, len: len32 };
            let encoded = match encode_args(&args) {
                Ok(bytes) => bytes,
                Err(err) => return Poll::Ready(Some(Err(err))),
            };
            let fut = match this.kind {
                ReaderKind::Strong => {
                    DriverFuture::<channel_strong_read_frame::Module, RkyvDecoder<IoFrame>>::new(
                        &encoded,
                        this.chunk_size + RKYV_VEC_OVERHEAD + 8,
                        RkyvDecoder::new(),
                    )
                    .map(ReaderInflight::Strong)
                }
                ReaderKind::Weak => {
                    DriverFuture::<channel_weak_read_frame::Module, RkyvDecoder<IoFrame>>::new(
                        &encoded,
                        this.chunk_size + RKYV_VEC_OVERHEAD + 8,
                        RkyvDecoder::new(),
                    )
                    .map(ReaderInflight::Weak)
                }
            };
            let fut = match fut {
                Ok(fut) => fut,
                Err(err) => return Poll::Ready(Some(Err(err))),
            };
            this.inflight = Some(fut);
        }

        let fut = match this.inflight.as_mut() {
            Some(fut) => fut,
            None => return Poll::Ready(Some(Err(DriverError::InvalidArgument))),
        };

        match fut.poll(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(res) => {
                this.inflight = None;

                match res {
                    Ok(frame) if frame.payload.is_empty() => Poll::Ready(None),
                    r => Poll::Ready(Some(r)),
                }
            }
        }
    }
}

impl FromHandle for Reader {
    type Handles = GuestResourceId;

    unsafe fn from_handle(handle: Self::Handles) -> Self {
        Self {
            handle,
            inflight: None,
            chunk_size: DEFAULT_CHUNK_SIZE as usize,
            kind: ReaderKind::Strong,
        }
    }
}

impl Writer {
    async fn attach(channel: ChannelHandle) -> Result<Self, DriverError> {
        let channel = guest_handle(channel)?;
        let args = encode_args(&channel)?;
        let handle = DriverFuture::<writer_create::Module, RkyvDecoder<GuestUint>>::new(
            &args,
            8,
            RkyvDecoder::new(),
        )?
        .await?;
        Ok(Self {
            handle: GuestResourceId::from(handle),
            kind: WriterKind::Strong,
            inflight: None,
        })
    }

    async fn attach_weak(channel: ChannelHandle) -> Result<Self, DriverError> {
        let channel = guest_handle(channel)?;
        let args = encode_args(&channel)?;
        let handle = DriverFuture::<weak_writer_create::Module, RkyvDecoder<GuestUint>>::new(
            &args,
            8,
            RkyvDecoder::new(),
        )?
        .await?;
        Ok(Self {
            handle: GuestResourceId::from(handle),
            kind: WriterKind::Weak,
            inflight: None,
        })
    }

    /// Downgrade the writer's handle to a compatible capability class.
    ///
    /// This is typically used when the host needs to transform a strong writer handle into a
    /// form that can be stored or shared.
    pub async fn downgrade(&mut self) -> Result<(), DriverError> {
        let handle = guest_handle(self.handle)?;
        let args = encode_args(&handle)?;
        let handle = DriverFuture::<writer_downgrade::Module, RkyvDecoder<GuestUint>>::new(
            &args,
            8,
            RkyvDecoder::new(),
        )?
        .await?;
        self.handle = GuestResourceId::from(handle);
        self.kind = WriterKind::Weak;
        Ok(())
    }
}

impl WriterInflight {
    fn poll(&mut self, cx: &mut Context<'_>) -> Poll<Result<GuestUint, DriverError>> {
        match self {
            WriterInflight::Strong(fut) => Pin::new(fut).poll(cx),
            WriterInflight::Weak(fut) => Pin::new(fut).poll(cx),
        }
    }
}

impl Sink<Vec<u8>> for Writer {
    type Error = DriverError;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        let this = self.get_mut();
        match this.inflight.as_mut() {
            Some(fut) => match fut.poll(cx) {
                Poll::Pending => Poll::Pending,
                Poll::Ready(result) => {
                    this.inflight = None;
                    Poll::Ready(result.map(|_| ()))
                }
            },
            None => Poll::Ready(Ok(())),
        }
    }

    fn start_send(mut self: Pin<&mut Self>, item: Vec<u8>) -> Result<(), Self::Error> {
        if self.inflight.is_some() {
            return Err(DriverError::InvalidArgument);
        }

        if item.is_empty() {
            return Ok(());
        }

        let handle = guest_handle(self.handle)?;
        let args = IoWrite {
            handle,
            payload: item,
        };
        let encoded = encode_args(&args)?;
        let fut = match self.kind {
            WriterKind::Strong => {
                WriterInflight::Strong(DriverFuture::<
                    channel_strong_write_frame::Module,
                    RkyvDecoder<GuestUint>,
                >::new(&encoded, 8, RkyvDecoder::new())?)
            }
            WriterKind::Weak => {
                WriterInflight::Weak(DriverFuture::<
                    channel_weak_write_frame::Module,
                    RkyvDecoder<GuestUint>,
                >::new(&encoded, 8, RkyvDecoder::new())?)
            }
        };
        self.inflight = Some(fut);

        Ok(())
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        let poll = match self.as_mut().get_mut().inflight.as_mut() {
            Some(fut) => fut.poll(cx),
            None => return Poll::Ready(Ok(())),
        };

        match poll {
            Poll::Pending => Poll::Pending,
            Poll::Ready(result) => {
                self.as_mut().get_mut().inflight = None;
                Poll::Ready(result.map(|_| ()))
            }
        }
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.poll_flush(cx)
    }
}

impl FromHandle for Writer {
    type Handles = GuestResourceId;

    unsafe fn from_handle(handle: Self::Handles) -> Self {
        Self {
            handle,
            kind: WriterKind::Strong,
            inflight: None,
        }
    }
}

fn guest_handle(handle: GuestResourceId) -> Result<GuestUint, DriverError> {
    GuestUint::try_from(handle).map_err(|_| DriverError::InvalidArgument)
}

driver_module!(
    reader_create,
    CHANNEL_STRONG_READER_CREATE,
    "selium::channel::strong_reader_create"
);
driver_module!(
    weak_reader_create,
    CHANNEL_WEAK_READER_CREATE,
    "selium::channel::weak_reader_create"
);
driver_module!(
    writer_create,
    CHANNEL_STRONG_WRITER_CREATE,
    "selium::channel::strong_writer_create"
);
driver_module!(
    weak_writer_create,
    CHANNEL_WEAK_WRITER_CREATE,
    "selium::channel::weak_writer_create"
);
driver_module!(
    writer_downgrade,
    CHANNEL_WRITER_DOWNGRADE,
    "selium::channel::writer_downgrade"
);
driver_module!(
    channel_strong_read_frame,
    CHANNEL_STRONG_READ,
    "selium::channel::strong_read"
);
driver_module!(
    channel_weak_read_frame,
    CHANNEL_WEAK_READ,
    "selium::channel::weak_read"
);
driver_module!(
    channel_strong_write_frame,
    CHANNEL_STRONG_WRITE,
    "selium::channel::strong_write"
);
driver_module!(
    channel_weak_write_frame,
    CHANNEL_WEAK_WRITE,
    "selium::channel::weak_write"
);
driver_module!(channel_create, CHANNEL_CREATE, "selium::channel::create");
driver_module!(channel_delete, CHANNEL_DELETE, "selium::channel::delete");
driver_module!(channel_drain, CHANNEL_DRAIN, "selium::channel::drain");
driver_module!(channel_attach, CHANNEL_ATTACH, "selium::channel::attach");
driver_module!(channel_detach, CHANNEL_DETACH, "selium::channel::detach");
driver_module!(channel_share, CHANNEL_SHARE, "selium::channel::share");

#[cfg(test)]
mod tests {
    use super::*;
    use selium_abi::decode_rkyv;

    #[test]
    fn channel_read_round_trips() {
        let read = IoRead {
            handle: 0x44332211,
            len: 0x88776655,
        };
        let bytes = encode_args(&read).expect("encode");
        let decoded = decode_rkyv::<IoRead>(&bytes).expect("decode");
        assert_eq!(decoded, read);
    }

    #[test]
    fn encode_write_args_serializes_payload() {
        let args = IoWrite {
            handle: 5,
            payload: b"hello".to_vec(),
        };
        let bytes = encode_args(&args).expect("encode");
        let decoded = decode_rkyv::<IoWrite>(&bytes).expect("decode");
        assert_eq!(decoded, args);
    }
}
