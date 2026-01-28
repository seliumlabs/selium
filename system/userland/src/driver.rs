//! Guest-side driver plumbing.
//!
//! Selium guest APIs are implemented in terms of *drivers* (hostcalls exposed to guests). This
//! module provides [`DriverFuture`], which wraps each driver's `create/poll/drop` hooks into an
//! ergonomic `async` future, plus helpers for serialising driver arguments.
//!
//! # Examples
//! ```
//! use selium_userland::io::{DriverError, encode_args};
//!
//! fn main() -> Result<(), DriverError> {
//!     let bytes = encode_args(&42u32)?;
//!     assert!(!bytes.is_empty());
//!     Ok(())
//! }
//! ```

use core::{marker::PhantomData, slice};
use std::{
    future::Future,
    io,
    pin::Pin,
    task::{Context, Poll},
};

use selium_abi::{
    DRIVER_ERROR_MESSAGE_CODE, DriverPollResult, GuestInt, GuestUint, RkyvEncode,
    decode_driver_error_message, decode_rkyv, driver_decode_result, encode_rkyv,
};
use thiserror::Error;

use crate::r#async;

/// Estimated overhead of a `Vec<u8>` when rkyv archives it.
pub const RKYV_VEC_OVERHEAD: usize = 16;
/// Minimum buffer capacity reserved for driver replies.
///
/// The host may return human-readable error strings; this value keeps common error responses from
/// reallocating.
pub const MIN_RESULT_CAPACITY: usize = 256;

/// Guest pointer type used by Selium driver hooks.
pub type DriverInt = GuestInt;
/// Guest integer type used by Selium driver hooks.
pub type DriverUint = GuestUint;

/// Contract implemented by each host driver exposed to guests.
///
/// Implementations simply forward to the `selium::async` FFIs; business logic uses the
/// type-safe [`DriverFuture`] wrapper instead of touching raw handles.
pub trait DriverModule {
    /// Create a new driver handle.
    ///
    /// # Safety
    /// `args_ptr..args_ptr+args_len` must describe a readable byte range in the guest's linear
    /// memory for the duration of this call.
    unsafe fn create(args_ptr: DriverInt, args_len: DriverUint) -> DriverUint;
    /// Poll an existing driver handle.
    ///
    /// # Safety
    /// - `result_ptr..result_ptr+result_len` must describe a writable byte range in the guest's
    ///   linear memory for the duration of this call.
    /// - `task_id` must be a valid identifier obtained from the Selium async runtime.
    unsafe fn poll(
        handle: DriverUint,
        task_id: DriverUint,
        result_ptr: DriverInt,
        result_len: DriverUint,
    ) -> DriverUint;
    /// Drop a driver handle, optionally writing a final result payload.
    ///
    /// # Safety
    /// `result_ptr..result_ptr+result_len` must describe a writable byte range in the guest's
    /// linear memory for the duration of this call.
    unsafe fn drop(handle: DriverUint, result_ptr: DriverInt, result_len: DriverUint)
    -> DriverUint;
}

/// Decodes the bytes returned by a driver into a concrete output type.
pub trait DriverDecoder: Unpin {
    type Output;
    fn decode(&mut self, bytes: &[u8]) -> Result<Self::Output, DriverError>;
}

/// Decoder that deserialises an rkyv payload into the requested type.
pub struct RkyvDecoder<T> {
    _marker: PhantomData<T>,
}

impl<T> RkyvDecoder<T> {
    /// Create a new decoder instance.
    pub fn new() -> Self {
        Self {
            _marker: PhantomData,
        }
    }
}

impl<T> Default for RkyvDecoder<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> DriverDecoder for RkyvDecoder<T>
where
    T: rkyv::Archive + Sized + Unpin,
    for<'a> T::Archived: 'a
        + rkyv::Deserialize<T, rkyv::api::high::HighDeserializer<rkyv::rancor::Error>>
        + rkyv::bytecheck::CheckBytes<rkyv::api::high::HighValidator<'a, rkyv::rancor::Error>>,
{
    type Output = T;

    fn decode(&mut self, bytes: &[u8]) -> Result<Self::Output, DriverError> {
        decode_rkyv_value(bytes)
    }
}

/// Generic error returned by host driver invocations.
#[derive(Debug, Error)]
pub enum DriverError {
    /// The driver returned a structured error string.
    #[error("driver error: {0}")]
    Driver(String),
    /// The kernel returned a numeric error code.
    #[error("kernel error: {0}")]
    Kernel(DriverUint),
    /// The caller supplied invalid arguments (for example, a length overflow).
    #[error("invalid argument")]
    InvalidArgument,
}

impl From<DriverError> for io::Error {
    fn from(value: DriverError) -> Self {
        match value {
            DriverError::Driver(msg) => io::Error::other(msg),
            DriverError::Kernel(code) => {
                io::Error::from_raw_os_error(i32::try_from(-(code as i64)).unwrap_or(-1))
            }
            DriverError::InvalidArgument => {
                io::Error::new(io::ErrorKind::InvalidInput, "invalid argument")
            }
        }
    }
}

/// Encode a driver argument value using Selium's rkyv configuration.
pub fn encode_args<T: RkyvEncode>(value: &T) -> Result<Vec<u8>, DriverError> {
    encode_rkyv(value).map_err(|err| DriverError::Driver(err.to_string()))
}

fn decode_rkyv_value<T>(bytes: &[u8]) -> Result<T, DriverError>
where
    T: rkyv::Archive + Sized,
    for<'a> T::Archived: 'a
        + rkyv::Deserialize<T, rkyv::api::high::HighDeserializer<rkyv::rancor::Error>>
        + rkyv::bytecheck::CheckBytes<rkyv::api::high::HighValidator<'a, rkyv::rancor::Error>>,
{
    decode_rkyv(bytes).map_err(|err| DriverError::Driver(err.to_string()))
}

struct GuestPtr {
    raw: DriverInt,
}

impl GuestPtr {
    fn new(ptr: *const u8) -> Result<Self, DriverError> {
        #[cfg(target_arch = "wasm32")]
        {
            let addr = ptr as usize;
            let raw = DriverInt::try_from(addr).map_err(|_| DriverError::InvalidArgument)?;
            Ok(Self { raw })
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let raw = host_compat::register(ptr)?;
            Ok(Self { raw })
        }
    }

    fn raw(&self) -> DriverInt {
        self.raw
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl Drop for GuestPtr {
    fn drop(&mut self) {
        host_compat::unregister(self.raw);
    }
}

#[cfg(target_arch = "wasm32")]
impl Drop for GuestPtr {
    fn drop(&mut self) {}
}

fn guest_len(len: usize) -> Result<DriverUint, DriverError> {
    DriverUint::try_from(len).map_err(|_| DriverError::InvalidArgument)
}

fn host_len(value: DriverUint) -> Result<usize, DriverError> {
    usize::try_from(value).map_err(|_| DriverError::InvalidArgument)
}

/// Guest-side future that drives a host driver through create/poll/drop FFI hooks.
///
/// The future owns the kernel handle and guarantees `drop` semantics, so higher level code can
/// express driver operations with idiomatic async/await.
pub struct DriverFuture<M, D>
where
    M: DriverModule,
    D: DriverDecoder,
{
    handle: Option<DriverUint>,
    result: Vec<u8>,
    decoder: D,
    _marker: PhantomData<M>,
}

impl<M, D> DriverFuture<M, D>
where
    M: DriverModule,
    D: DriverDecoder,
{
    /// Create a new future by calling the driver's `create` hook with the supplied arguments.
    ///
    /// `capacity` describes the expected maximum reply size and is clamped to
    /// [`MIN_RESULT_CAPACITY`].
    pub fn new(args: &[u8], capacity: usize, decoder: D) -> Result<Self, DriverError> {
        let len = guest_len(args.len())?;
        let ptr = GuestPtr::new(args.as_ptr())?;
        let handle = unsafe { M::create(ptr.raw(), len) };

        let cap = capacity.max(MIN_RESULT_CAPACITY);
        Ok(Self {
            handle: Some(handle),
            result: vec![0; cap],
            decoder,
            _marker: core::marker::PhantomData,
        })
    }

    fn poll_inner(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<D::Output, DriverError>> {
        let handle = match self.handle {
            Some(handle) => handle,
            None => return Poll::Ready(Err(DriverError::InvalidArgument)),
        };

        let task_id = r#async::register(cx);
        let capacity = match guest_len(self.result.len()) {
            Ok(len) => len,
            Err(err) => return Poll::Ready(Err(err)),
        };
        let ptr = match GuestPtr::new(self.result.as_mut_ptr()) {
            Ok(ptr) => ptr,
            Err(err) => return Poll::Ready(Err(err)),
        };
        let rc = unsafe { M::poll(handle, task_id, ptr.raw(), capacity) };

        match driver_decode_result(rc) {
            DriverPollResult::Pending => Poll::Pending,
            DriverPollResult::Error(code) => {
                self.handle = None;
                if code == DRIVER_ERROR_MESSAGE_CODE {
                    let msg = decode_driver_error(&self.result);
                    Poll::Ready(Err(DriverError::Driver(msg)))
                } else {
                    Poll::Ready(Err(DriverError::Kernel(code)))
                }
            }
            DriverPollResult::Ready(value) => {
                if value > capacity {
                    self.handle = None;
                    return Poll::Ready(Err(DriverError::Kernel(value)));
                }

                let used = match host_len(value) {
                    Ok(len) => len,
                    Err(err) => {
                        self.handle = None;
                        return Poll::Ready(Err(err));
                    }
                };
                if used > self.result.len() {
                    self.handle = None;
                    return Poll::Ready(Err(DriverError::InvalidArgument));
                }

                self.handle = None;
                let ptr = self.result.as_ptr();
                let output = {
                    let bytes = unsafe { slice::from_raw_parts(ptr, used) };
                    let decoded = self.decoder.decode(bytes);
                    if let Err(DriverError::Driver(ref msg)) = decoded {
                        tracing::warn!(
                            "driver decode failed (module={}, used={}): {msg}",
                            std::any::type_name::<M>(),
                            used
                        );
                    }
                    decoded
                };
                Poll::Ready(output)
            }
        }
    }
}

impl<M, D> Future for DriverFuture<M, D>
where
    M: DriverModule,
    D: DriverDecoder,
{
    type Output = Result<D::Output, DriverError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.poll_inner(cx)
    }
}

impl<M, D> Drop for DriverFuture<M, D>
where
    M: DriverModule,
    D: DriverDecoder,
{
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take()
            && let (Ok(len), Ok(ptr)) = (
                guest_len(self.result.len()),
                GuestPtr::new(self.result.as_mut_ptr()),
            )
        {
            let _ = unsafe { M::drop(handle, ptr.raw(), len) };
        }
    }
}

impl<M, D> Unpin for DriverFuture<M, D>
where
    M: DriverModule,
    D: DriverDecoder,
{
}

fn decode_driver_error(buf: &[u8]) -> String {
    decode_driver_error_message(buf).unwrap_or_else(|_| "driver error".to_string())
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) mod host_compat {
    use super::{DriverError, DriverInt};
    use std::{
        collections::HashMap,
        sync::{Mutex, OnceLock},
    };

    struct PtrRegistry {
        next: DriverInt,
        entries: HashMap<DriverInt, usize>,
    }

    impl PtrRegistry {
        fn new() -> Self {
            Self {
                next: 1,
                entries: HashMap::new(),
            }
        }
    }

    impl Default for PtrRegistry {
        fn default() -> Self {
            Self::new()
        }
    }

    static REGISTRY: OnceLock<Mutex<PtrRegistry>> = OnceLock::new();

    fn registry() -> &'static Mutex<PtrRegistry> {
        REGISTRY.get_or_init(|| Mutex::new(PtrRegistry::new()))
    }

    pub fn register(ptr: *const u8) -> Result<DriverInt, DriverError> {
        let mut guard = registry().lock().expect("pointer registry poisoned");
        let id = guard.next;
        guard.next = guard
            .next
            .checked_add(1)
            .ok_or(DriverError::InvalidArgument)?;
        guard.entries.insert(id, ptr as usize);
        Ok(id)
    }

    pub fn unregister(id: DriverInt) {
        if let Some(registry) = REGISTRY.get()
            && let Ok(mut guard) = registry.lock()
        {
            guard.entries.remove(&id);
        }
    }

    #[cfg(all(not(target_arch = "wasm32"), test))]
    pub unsafe fn ptr_from_guest(id: DriverInt) -> *const u8 {
        registry()
            .lock()
            .expect("pointer registry poisoned")
            .entries
            .get(&id)
            .copied()
            .map(|addr| addr as *const u8)
            .unwrap_or(core::ptr::null())
    }

    #[cfg(all(not(target_arch = "wasm32"), test))]
    pub unsafe fn ptr_from_guest_mut(id: DriverInt) -> *mut u8 {
        (unsafe { ptr_from_guest(id) }) as *mut u8
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
pub(crate) mod test_driver {
    use std::{
        collections::{HashMap, VecDeque},
        mem, slice,
        sync::{Mutex, OnceLock},
        time::{Instant, SystemTime, UNIX_EPOCH},
    };

    use selium_abi::{
        DRIVER_RESULT_PENDING, GuestInt, GuestUint, IoFrame, IoRead, IoWrite, decode_rkyv,
        driver_encode_error, driver_encode_ready, encode_rkyv,
    };

    use super::{DriverError, RkyvEncode, host_compat};
    use crate::r#async;

    type ChannelHandle = GuestUint;
    type ReaderHandle = GuestUint;
    type WriterHandle = GuestUint;

    enum Operation {
        Return(Vec<u8>),
        Read(IoRead),
        Write(IoWrite),
    }

    struct ChannelState {
        queue: VecDeque<IoFrame>,
        waiters: Vec<GuestUint>,
    }

    struct State {
        next_op: GuestUint,
        next_channel: ChannelHandle,
        next_writer_id: u16,
        operations: HashMap<GuestUint, Operation>,
        channels: HashMap<ChannelHandle, ChannelState>,
        readers: HashMap<ReaderHandle, ChannelHandle>,
        writers: HashMap<WriterHandle, (ChannelHandle, u16)>,
    }

    impl State {
        fn new() -> Self {
            Self {
                next_op: 1,
                next_channel: 1,
                next_writer_id: 1,
                operations: HashMap::new(),
                channels: HashMap::new(),
                readers: HashMap::new(),
                writers: HashMap::new(),
            }
        }

        fn insert_op(&mut self, op: Operation) -> GuestUint {
            let handle = self.next_op;
            self.next_op = self.next_op.saturating_add(1);
            self.operations.insert(handle, op);
            handle
        }

        fn channel_mut(&mut self, handle: ChannelHandle) -> Option<&mut ChannelState> {
            self.channels.get_mut(&handle)
        }

        fn wake_waiters(&mut self, channel: ChannelHandle) {
            if let Some(state) = self.channels.get_mut(&channel) {
                let waiters = mem::take(&mut state.waiters);
                for waiter in waiters {
                    r#async::wake(waiter);
                }
            }
        }
    }

    fn state() -> &'static Mutex<State> {
        static STATE: OnceLock<Mutex<State>> = OnceLock::new();
        STATE.get_or_init(|| Mutex::new(State::new()))
    }

    fn decode_args(ptr: GuestInt, len: GuestUint) -> Result<&'static [u8], DriverError> {
        let len = usize::try_from(len).map_err(|_| DriverError::InvalidArgument)?;
        let ptr = unsafe { host_compat::ptr_from_guest(ptr) };
        if ptr.is_null() {
            return Err(DriverError::InvalidArgument);
        }
        unsafe { Ok(slice::from_raw_parts(ptr, len)) }
    }

    fn encode<T: RkyvEncode>(value: &T) -> Result<Vec<u8>, DriverError> {
        encode_rkyv(value).map_err(|err| DriverError::Driver(err.to_string()))
    }

    fn unix_ms() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64
    }

    fn monotonic_ms() -> u64 {
        static START: OnceLock<Instant> = OnceLock::new();
        START.get_or_init(Instant::now).elapsed().as_millis() as u64
    }

    pub fn create(module: &str, args_ptr: GuestInt, args_len: GuestUint) -> GuestUint {
        let mut guard = match state().lock() {
            Ok(guard) => guard,
            Err(_) => return 0,
        };
        match module {
            selium_abi::hostcall_name!(CHANNEL_CREATE) => {
                let args = match decode_args(args_ptr, args_len) {
                    Ok(buf) => buf,
                    Err(_) => return 0,
                };
                let _: selium_abi::ChannelCreate = match decode_rkyv(args) {
                    Ok(value) => value,
                    Err(_) => return 0,
                };
                let handle = guard.next_channel;
                guard.next_channel = guard.next_channel.saturating_add(1);
                guard.channels.insert(
                    handle,
                    ChannelState {
                        queue: VecDeque::new(),
                        waiters: Vec::new(),
                    },
                );
                match encode(&handle) {
                    Ok(bytes) => guard.insert_op(Operation::Return(bytes)),
                    Err(_) => 0,
                }
            }
            selium_abi::hostcall_name!(CHANNEL_STRONG_WRITER_CREATE) => {
                let args = match decode_args(args_ptr, args_len) {
                    Ok(buf) => buf,
                    Err(_) => return 0,
                };
                let channel: ChannelHandle = match decode_rkyv(args) {
                    Ok(value) => value,
                    Err(_) => return 0,
                };
                let writer_handle = guard.next_op.saturating_add(1000);
                let writer_id = guard.next_writer_id;
                guard.next_writer_id = guard.next_writer_id.saturating_add(1);
                guard.writers.insert(writer_handle, (channel, writer_id));
                match encode(&writer_handle) {
                    Ok(bytes) => guard.insert_op(Operation::Return(bytes)),
                    Err(_) => 0,
                }
            }
            selium_abi::hostcall_name!(CHANNEL_WEAK_WRITER_CREATE) => {
                let args = match decode_args(args_ptr, args_len) {
                    Ok(buf) => buf,
                    Err(_) => return 0,
                };
                let channel: ChannelHandle = match decode_rkyv(args) {
                    Ok(value) => value,
                    Err(_) => return 0,
                };
                let writer_handle = guard.next_op.saturating_add(1000);
                let writer_id = guard.next_writer_id;
                guard.next_writer_id = guard.next_writer_id.saturating_add(1);
                guard.writers.insert(writer_handle, (channel, writer_id));
                match encode(&writer_handle) {
                    Ok(bytes) => guard.insert_op(Operation::Return(bytes)),
                    Err(_) => 0,
                }
            }
            selium_abi::hostcall_name!(CHANNEL_STRONG_READER_CREATE) => {
                let args = match decode_args(args_ptr, args_len) {
                    Ok(buf) => buf,
                    Err(_) => return 0,
                };
                let channel: ChannelHandle = match decode_rkyv(args) {
                    Ok(value) => value,
                    Err(_) => return 0,
                };
                let reader_handle = guard.next_op.saturating_add(2000);
                guard.readers.insert(reader_handle, channel);
                match encode(&reader_handle) {
                    Ok(bytes) => guard.insert_op(Operation::Return(bytes)),
                    Err(_) => 0,
                }
            }
            selium_abi::hostcall_name!(CHANNEL_WEAK_READER_CREATE) => {
                let args = match decode_args(args_ptr, args_len) {
                    Ok(buf) => buf,
                    Err(_) => return 0,
                };
                let channel: ChannelHandle = match decode_rkyv(args) {
                    Ok(value) => value,
                    Err(_) => return 0,
                };
                let reader_handle = guard.next_op.saturating_add(2000);
                guard.readers.insert(reader_handle, channel);
                match encode(&reader_handle) {
                    Ok(bytes) => guard.insert_op(Operation::Return(bytes)),
                    Err(_) => 0,
                }
            }
            selium_abi::hostcall_name!(CHANNEL_STRONG_WRITE)
            | selium_abi::hostcall_name!(CHANNEL_WEAK_WRITE) => {
                let args = match decode_args(args_ptr, args_len) {
                    Ok(buf) => buf,
                    Err(_) => return 0,
                };
                let write: IoWrite = match decode_rkyv(args) {
                    Ok(value) => value,
                    Err(_) => return 0,
                };
                guard.insert_op(Operation::Write(write))
            }
            selium_abi::hostcall_name!(CHANNEL_STRONG_READ)
            | selium_abi::hostcall_name!(CHANNEL_WEAK_READ) => {
                let args = match decode_args(args_ptr, args_len) {
                    Ok(buf) => buf,
                    Err(_) => return 0,
                };
                let read: IoRead = match decode_rkyv(args) {
                    Ok(value) => value,
                    Err(_) => return 0,
                };
                guard.insert_op(Operation::Read(read))
            }
            selium_abi::hostcall_name!(TIME_NOW) => {
                let now = selium_abi::TimeNow {
                    unix_ms: unix_ms(),
                    monotonic_ms: monotonic_ms(),
                };
                match encode(&now) {
                    Ok(bytes) => guard.insert_op(Operation::Return(bytes)),
                    Err(_) => 0,
                }
            }
            _ => guard.insert_op(Operation::Return(Vec::new())),
        }
    }

    pub fn poll(
        _module: &str,
        handle: GuestUint,
        task_id: GuestUint,
        result_ptr: GuestInt,
        result_len: GuestUint,
    ) -> GuestUint {
        let mut guard = match state().lock() {
            Ok(guard) => guard,
            Err(_) => return driver_encode_error(1),
        };
        let Some(op) = guard.operations.remove(&handle) else {
            return driver_encode_error(1);
        };
        let capacity = usize::try_from(result_len).unwrap_or_default();
        let ptr = unsafe { host_compat::ptr_from_guest_mut(result_ptr) };
        if ptr.is_null() {
            return driver_encode_error(1);
        }

        match op {
            Operation::Return(bytes) => {
                let len = bytes.len().min(capacity);
                unsafe { core::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr, len) };
                driver_encode_ready(GuestUint::try_from(len).unwrap_or(0)).unwrap_or(0)
            }
            Operation::Write(write) => {
                let (channel, writer_id) =
                    if let Some((channel, writer_id)) = guard.writers.get(&write.handle).copied() {
                        (channel, writer_id)
                    } else if guard.channels.contains_key(&write.handle) {
                        let writer_id = guard.next_writer_id;
                        guard.next_writer_id = guard.next_writer_id.saturating_add(1);
                        guard
                            .writers
                            .insert(write.handle, (write.handle, writer_id));
                        (write.handle, writer_id)
                    } else {
                        guard.operations.remove(&handle);
                        return driver_encode_error(1);
                    };
                if let Some(chan) = guard.channel_mut(channel) {
                    chan.queue.push_back(IoFrame {
                        writer_id,
                        payload: write.payload.clone(),
                    });
                    guard.wake_waiters(channel);
                }
                let ack = encode(&0u32).unwrap_or_default();
                let len = ack.len().min(capacity);
                unsafe { core::ptr::copy_nonoverlapping(ack.as_ptr(), ptr, len) };
                driver_encode_ready(GuestUint::try_from(len).unwrap_or(0)).unwrap_or(0)
            }
            Operation::Read(read) => {
                let channel = if let Some(channel) = guard.readers.get(&read.handle).copied() {
                    channel
                } else if guard.channels.contains_key(&read.handle) {
                    guard.readers.insert(read.handle, read.handle);
                    read.handle
                } else {
                    guard.operations.remove(&handle);
                    return driver_encode_error(1);
                };
                let pending = if let Some(chan) = guard.channel_mut(channel) {
                    if let Some(frame) = chan.queue.pop_front() {
                        let payload = if read.len == 0 {
                            Vec::new()
                        } else {
                            let max = usize::try_from(read.len).unwrap_or(frame.payload.len());
                            frame.payload.into_iter().take(max).collect()
                        };
                        let encoded_frame = encode(&IoFrame {
                            writer_id: frame.writer_id,
                            payload,
                        })
                        .unwrap_or_default();
                        let len = encoded_frame.len().min(capacity);
                        unsafe { core::ptr::copy_nonoverlapping(encoded_frame.as_ptr(), ptr, len) };
                        return driver_encode_ready(GuestUint::try_from(len).unwrap_or(0))
                            .unwrap_or(0);
                    }
                    chan.waiters.push(task_id);
                    true
                } else {
                    return driver_encode_error(1);
                };
                if pending {
                    guard.operations.insert(handle, Operation::Read(read));
                }
                DRIVER_RESULT_PENDING
            }
        }
    }

    pub fn drop(
        _module: &str,
        handle: GuestUint,
        _result_ptr: GuestInt,
        _result_len: GuestUint,
    ) -> GuestUint {
        if let Ok(mut guard) = state().lock() {
            guard.operations.remove(&handle);
        }
        0
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use futures::task::noop_waker;
    use selium_abi::{DRIVER_RESULT_PENDING, driver_encode_error, driver_encode_ready};
    use std::{
        pin::Pin,
        sync::atomic::{AtomicU32, Ordering},
    };

    #[cfg(not(target_arch = "wasm32"))]
    use super::host_compat;

    #[cfg(target_arch = "wasm32")]
    unsafe fn test_ptr_mut(ptr: DriverInt) -> *mut u8 {
        ptr as *mut u8
    }

    #[cfg(not(target_arch = "wasm32"))]
    unsafe fn test_ptr_mut(ptr: DriverInt) -> *mut u8 {
        unsafe { host_compat::ptr_from_guest_mut(ptr) }
    }

    fn run_ready<F>(fut: F) -> F::Output
    where
        F: Future,
    {
        let waker = noop_waker();
        let mut cx = Context::from_waker(&waker);
        let mut fut = Box::pin(fut);
        loop {
            match fut.as_mut().poll(&mut cx) {
                Poll::Ready(val) => break val,
                Poll::Pending => continue,
            }
        }
    }

    struct ReadyModule;

    impl DriverModule for ReadyModule {
        unsafe fn create(_args_ptr: DriverInt, _args_len: DriverUint) -> DriverUint {
            1
        }

        unsafe fn poll(
            _handle: DriverUint,
            _task_id: DriverUint,
            result_ptr: DriverInt,
            _result_len: DriverUint,
        ) -> DriverUint {
            let payload = b"ok";
            unsafe {
                core::ptr::copy_nonoverlapping(
                    payload.as_ptr(),
                    test_ptr_mut(result_ptr),
                    payload.len(),
                );
            }
            let len = DriverUint::try_from(payload.len()).unwrap();
            driver_encode_ready(len).expect("payload length fits")
        }

        unsafe fn drop(
            _handle: DriverUint,
            _result_ptr: DriverInt,
            _result_len: DriverUint,
        ) -> DriverUint {
            0
        }
    }

    struct StrDecoder;

    impl DriverDecoder for StrDecoder {
        type Output = String;

        fn decode(&mut self, bytes: &[u8]) -> Result<Self::Output, DriverError> {
            Ok(std::str::from_utf8(bytes).unwrap().to_string())
        }
    }

    #[test]
    fn driver_future_yields_ready_bytes() {
        let fut = DriverFuture::<ReadyModule, StrDecoder>::new(&[], 4, StrDecoder).unwrap();
        let out = run_ready(fut).unwrap();
        assert_eq!(out, "ok");
    }

    struct DriverErrorModule;

    impl DriverModule for DriverErrorModule {
        unsafe fn create(_args_ptr: DriverInt, _args_len: DriverUint) -> DriverUint {
            2
        }

        unsafe fn poll(
            _handle: DriverUint,
            _task_id: DriverUint,
            result_ptr: DriverInt,
            _result_len: DriverUint,
        ) -> DriverUint {
            let encoded = selium_abi::encode_driver_error_message("boom").expect("encode");
            unsafe {
                core::ptr::copy_nonoverlapping(
                    encoded.as_ptr(),
                    test_ptr_mut(result_ptr),
                    encoded.len(),
                )
            };
            driver_encode_error(DRIVER_ERROR_MESSAGE_CODE)
        }

        unsafe fn drop(
            _handle: DriverUint,
            _result_ptr: DriverInt,
            _result_len: DriverUint,
        ) -> DriverUint {
            0
        }
    }

    struct UnitDecoder;

    impl DriverDecoder for UnitDecoder {
        type Output = ();

        fn decode(&mut self, _bytes: &[u8]) -> Result<Self::Output, DriverError> {
            Ok(())
        }
    }

    #[test]
    fn driver_future_reports_driver_error() {
        let fut =
            DriverFuture::<DriverErrorModule, UnitDecoder>::new(&[], 32, UnitDecoder).unwrap();
        let err = run_ready(fut).unwrap_err();
        match err {
            DriverError::Driver(msg) => assert_eq!(msg, "boom"),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    struct PendingModule;

    static DROPS: AtomicU32 = AtomicU32::new(0);

    impl DriverModule for PendingModule {
        unsafe fn create(_args_ptr: DriverInt, _args_len: DriverUint) -> DriverUint {
            3
        }

        unsafe fn poll(
            _handle: DriverUint,
            _task_id: DriverUint,
            _result_ptr: DriverInt,
            _result_len: DriverUint,
        ) -> DriverUint {
            DRIVER_RESULT_PENDING
        }

        unsafe fn drop(
            _handle: DriverUint,
            _result_ptr: DriverInt,
            _result_len: DriverUint,
        ) -> DriverUint {
            DROPS.fetch_add(1, Ordering::SeqCst);
            0
        }
    }

    #[test]
    fn driver_future_drops_pending_handle() {
        let mut fut = DriverFuture::<PendingModule, UnitDecoder>::new(&[], 4, UnitDecoder).unwrap();
        let waker = noop_waker();
        let mut cx = Context::from_waker(&waker);
        assert!(matches!(Pin::new(&mut fut).poll(&mut cx), Poll::Pending));
        drop(fut);
        assert_eq!(DROPS.load(Ordering::SeqCst), 1);
    }
}
