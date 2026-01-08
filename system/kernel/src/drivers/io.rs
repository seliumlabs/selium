use std::{future::ready, sync::Arc};

use selium_abi::hostcalls::Hostcall;
use selium_abi::{GuestUint, IoFrame, IoRead, IoWrite};
use wasmtime::Caller;

use crate::{
    guest_data::{GuestError, GuestResult},
    operation::{Contract, Operation},
    registry::{InstanceRegistry, ResourceHandle, ResourceType},
};

/// The capabilities that any subsystem implementation needs to provide
pub trait IoCapability {
    type Handle: Send;
    type Reader: Send + Unpin;
    type Writer: Send + Unpin;
    type Error: Into<GuestError>;

    /// Create a new writer for the given handle
    fn new_writer(&self, handle: &Self::Handle) -> Result<Self::Writer, Self::Error>;

    /// Create a new reader for the given handle
    fn new_reader(&self, handle: &Self::Handle) -> Result<Self::Reader, Self::Error>;

    /// Read up to `len` bytes
    fn read(
        &self,
        reader: &mut Self::Reader,
        len: usize,
    ) -> impl Future<Output = Result<IoFrame, Self::Error>> + Send;

    /// Write the given bytes
    fn write(
        &self,
        writer: &mut Self::Writer,
        bytes: &[u8],
    ) -> impl Future<Output = Result<(), Self::Error>> + Send;
}

pub struct IoCreateReaderDriver<Impl>(Impl);
pub struct IoReadDriver<Impl>(Impl);
pub struct IoCreateWriterDriver<Impl>(Impl);
pub struct IoWriteDriver<Impl>(Impl);

impl<T> IoCapability for Arc<T>
where
    T: IoCapability,
{
    type Handle = T::Handle;
    type Reader = T::Reader;
    type Writer = T::Writer;
    type Error = T::Error;

    fn new_reader(&self, handle: &Self::Handle) -> Result<Self::Reader, Self::Error> {
        self.as_ref().new_reader(handle)
    }

    fn new_writer(&self, handle: &Self::Handle) -> Result<Self::Writer, Self::Error> {
        self.as_ref().new_writer(handle)
    }

    fn read(
        &self,
        reader: &mut Self::Reader,
        len: usize,
    ) -> impl Future<Output = Result<IoFrame, Self::Error>> {
        self.as_ref().read(reader, len)
    }

    fn write(
        &self,
        writer: &mut Self::Writer,
        bytes: &[u8],
    ) -> impl Future<Output = Result<(), Self::Error>> {
        self.as_ref().write(writer, bytes)
    }
}

impl<Impl> Contract for IoCreateReaderDriver<Impl>
where
    Impl: IoCapability + Clone + Send + 'static,
{
    type Input = GuestUint;
    type Output = GuestUint;

    fn to_future(
        &self,
        caller: &mut Caller<'_, InstanceRegistry>,
        input: Self::Input,
    ) -> impl Future<Output = GuestResult<Self::Output>> + 'static {
        let this = self.0.clone();
        let idx = caller
            .data()
            .entry(input as usize)
            .ok_or(GuestError::NotFound);
        let registry = caller.data().registry_arc();

        let result = (|| -> GuestResult<GuestUint> {
            let idx = idx?;
            let reader = registry
                .with(ResourceHandle::<Impl::Handle>::new(idx), move |handle| {
                    this.new_reader(handle)
                })
                .expect("Invalid resource id from InstanceRegistry")
                .map_err(Into::into)?;

            let slot = caller
                .data_mut()
                .insert(reader, None, ResourceType::Reader)
                .map_err(GuestError::from)?;
            if let Some(resource_id) = caller.data().entry(slot) {
                registry.record_parent(resource_id, idx);
            }
            GuestUint::try_from(slot).map_err(|_| GuestError::InvalidArgument)
        })();

        ready(result)
    }
}

impl<Impl> Contract for IoReadDriver<Impl>
where
    Impl: IoCapability + Clone + Send + 'static,
{
    type Input = IoRead;
    type Output = IoFrame;

    fn to_future(
        &self,
        caller: &mut Caller<'_, InstanceRegistry>,
        input: Self::Input,
    ) -> impl Future<Output = GuestResult<Self::Output>> + 'static {
        let this = self.0.clone();
        let idx = caller
            .data()
            .entry(input.handle as usize)
            .ok_or(GuestError::NotFound);
        let registry = caller.data().registry_arc();
        let len = input.len as usize;

        async move {
            registry
                .with_async(ResourceHandle::<Impl::Reader>::new(idx?), move |reader| {
                    Box::pin(async move { this.read(reader, len).await })
                })
                .await
                .expect("Invalid resource id from InstanceRegistry")
                .map_err(Into::into)
        }
    }
}

impl<Impl> Contract for IoCreateWriterDriver<Impl>
where
    Impl: IoCapability + Clone + Send + 'static,
{
    type Input = GuestUint;
    type Output = GuestUint;

    fn to_future(
        &self,
        caller: &mut Caller<'_, InstanceRegistry>,
        input: Self::Input,
    ) -> impl Future<Output = GuestResult<Self::Output>> + 'static {
        let this = self.0.clone();
        let idx = caller
            .data()
            .entry(input as usize)
            .ok_or(GuestError::NotFound);
        let registry = caller.data().registry_arc();

        let result = (|| -> GuestResult<GuestUint> {
            let idx = idx?;
            let writer = registry
                .with(ResourceHandle::<Impl::Handle>::new(idx), move |channel| {
                    this.new_writer(channel)
                })
                .expect("Invalid resource id from InstanceRegistry")
                .map_err(Into::into)?;

            let slot = caller
                .data_mut()
                .insert(writer, None, ResourceType::Writer)
                .map_err(GuestError::from)?;
            if let Some(resource_id) = caller.data().entry(slot) {
                registry.record_parent(resource_id, idx);
            }
            GuestUint::try_from(slot).map_err(|_| GuestError::InvalidArgument)
        })();

        ready(result)
    }
}

impl<Impl> Contract for IoWriteDriver<Impl>
where
    Impl: IoCapability + Clone + Send + 'static,
{
    type Input = IoWrite;
    type Output = GuestUint;

    fn to_future(
        &self,
        caller: &mut Caller<'_, InstanceRegistry>,
        input: Self::Input,
    ) -> impl Future<Output = GuestResult<Self::Output>> + 'static {
        let this = self.0.clone();
        let payload = input.payload;
        let idx = caller
            .data()
            .entry(input.handle as usize)
            .ok_or(GuestError::NotFound);
        let registry = caller.data().registry_arc();
        let payload_len = payload.len();

        async move {
            registry
                .with_async(ResourceHandle::<Impl::Writer>::new(idx?), move |writer| {
                    Box::pin(async move { this.write(writer, &payload).await })
                })
                .await
                .expect("Invalid resource id from InstanceRegistry")
                .map_err(Into::into)?;

            let count =
                GuestUint::try_from(payload_len).map_err(|_| GuestError::InvalidArgument)?;
            Ok(count)
        }
    }
}

pub fn create_reader_op<C>(
    cap: C,
    hostcall: &'static Hostcall<GuestUint, GuestUint>,
) -> Arc<Operation<IoCreateReaderDriver<C>>>
where
    C: IoCapability + Clone + Send + 'static,
{
    Operation::from_hostcall(IoCreateReaderDriver(cap), hostcall)
}

pub fn read_op<C>(
    cap: C,
    hostcall: &'static Hostcall<IoRead, IoFrame>,
) -> Arc<Operation<IoReadDriver<C>>>
where
    C: IoCapability + Clone + Send + 'static,
{
    Operation::from_hostcall(IoReadDriver(cap), hostcall)
}

pub fn create_writer_op<C>(
    cap: C,
    hostcall: &'static Hostcall<GuestUint, GuestUint>,
) -> Arc<Operation<IoCreateWriterDriver<C>>>
where
    C: IoCapability + Clone + Send + 'static,
{
    Operation::from_hostcall(IoCreateWriterDriver(cap), hostcall)
}

pub fn write_op<C>(
    cap: C,
    hostcall: &'static Hostcall<IoWrite, GuestUint>,
) -> Arc<Operation<IoWriteDriver<C>>>
where
    C: IoCapability + Clone + Send + 'static,
{
    Operation::from_hostcall(IoWriteDriver(cap), hostcall)
}
