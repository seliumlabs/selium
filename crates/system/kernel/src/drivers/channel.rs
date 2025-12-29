use std::{
    future::{Future, ready},
    io,
    pin::Pin,
    sync::Arc,
};

use wasmtime::Caller;

use crate::{
    drivers::io::{
        IoCapability, IoCreateReaderDriver, IoCreateWriterDriver, IoReadDriver, IoWriteDriver,
        create_reader_op, create_writer_op, read_op, write_op,
    },
    guest_data::{GuestError, GuestResult, GuestUint},
    operation::{Contract, Operation},
    registry::{InstanceRegistry, ResourceType},
};
use selium_abi::GuestResourceId;

type ChannelLifecycleOps<C> = (
    Arc<Operation<ChannelCreateDriver<C>>>,
    Arc<Operation<ChannelDeleteDriver<C>>>,
    Arc<Operation<ChannelDrainDriver<C>>>,
);

type ChannelHandoffOps = (
    Arc<Operation<ChannelExportDriver>>,
    Arc<Operation<ChannelAttachDriver>>,
    Arc<Operation<ChannelDetachDriver>>,
);

type FrameReadFuture<'a> = Pin<Box<dyn Future<Output = io::Result<(u16, Vec<u8>)>> + Send + 'a>>;

type ChannelReadOps<S, W> = (
    Arc<Operation<IoCreateReaderDriver<S>>>,
    Arc<Operation<IoCreateReaderDriver<W>>>,
    Arc<Operation<IoReadDriver<S>>>,
);

type ChannelWriteOps<S, W> = (
    Arc<Operation<IoCreateWriterDriver<S>>>,
    Arc<Operation<IoCreateWriterDriver<W>>>,
    Arc<Operation<IoWriteDriver<S>>>,
);

/// The capabilities that any subsystem implementation needs to provide
pub trait ChannelCapability: Send + Sync {
    type Channel: Send;
    type StrongWriter: Send + Unpin;
    type WeakWriter: Send + Unpin;
    type StrongReader: Send + Unpin;
    type WeakReader: Send + Unpin;
    type Error: Into<GuestError>;

    /// Create a new channel for transporting bytes
    fn create(&self, size: GuestUint) -> Result<Self::Channel, Self::Error>;

    /// Delete this channel
    fn delete(&self, channel: Self::Channel) -> Result<(), Self::Error>;

    /// Terminate this channel whilst allowing unfinished reads/writes to continue
    fn drain(&self, channel: &Self::Channel) -> Result<(), Self::Error>;

    /// Downgrade this strong writer to a weak variant
    fn downgrade_writer(&self, writer: Self::StrongWriter)
    -> Result<Self::WeakWriter, Self::Error>;

    /// Downgrade this strong writer to a weak variant
    fn downgrade_reader(&self, writer: Self::StrongReader)
    -> Result<Self::WeakReader, Self::Error>;

    #[doc(hidden)]
    fn ptr(&self, channel: &Self::Channel) -> String;
}

/// Reader capable of yielding whole frames with attribution.
pub trait FrameReadable {
    fn read_frame(&mut self, max_len: usize) -> FrameReadFuture<'_>;
}

pub struct ChannelCreateDriver<Impl>(Impl);
pub struct ChannelDeleteDriver<Impl>(Impl);
pub struct ChannelDrainDriver<Impl>(Impl);
pub struct ChannelDowngradeStrongWriterDriver<Impl>(Impl);
pub struct ChannelExportDriver;
pub struct ChannelAttachDriver;
pub struct ChannelDetachDriver;

impl<T> ChannelCapability for Arc<T>
where
    T: ChannelCapability + Send + Sync,
{
    type Channel = T::Channel;
    type StrongWriter = T::StrongWriter;
    type WeakWriter = T::WeakWriter;
    type StrongReader = T::StrongReader;
    type WeakReader = T::WeakReader;
    type Error = T::Error;

    fn create(&self, size: GuestUint) -> Result<Self::Channel, Self::Error> {
        self.as_ref().create(size)
    }

    fn delete(&self, channel: Self::Channel) -> Result<(), Self::Error> {
        self.as_ref().delete(channel)
    }

    fn drain(&self, channel: &Self::Channel) -> Result<(), Self::Error> {
        self.as_ref().drain(channel)
    }

    fn downgrade_writer(
        &self,
        writer: Self::StrongWriter,
    ) -> Result<Self::WeakWriter, Self::Error> {
        self.as_ref().downgrade_writer(writer)
    }

    fn downgrade_reader(
        &self,
        reader: Self::StrongReader,
    ) -> Result<Self::WeakReader, Self::Error> {
        self.as_ref().downgrade_reader(reader)
    }

    fn ptr(&self, channel: &Self::Channel) -> String {
        self.as_ref().ptr(channel)
    }
}

impl<Impl> Contract for ChannelCreateDriver<Impl>
where
    Impl: ChannelCapability + Clone + Send + 'static,
{
    type Input = GuestUint;
    type Output = GuestUint;

    fn to_future(
        &self,
        caller: &mut Caller<'_, InstanceRegistry>,
        size: Self::Input,
    ) -> impl Future<Output = GuestResult<Self::Output>> + 'static {
        let inner = self.0.clone();
        let registry = caller.data().registry_arc();

        let result = (|| -> GuestResult<GuestUint> {
            let channel = inner.create(size).map_err(Into::into)?;
            let ptr = inner.ptr(&channel);
            let slot = caller
                .data_mut()
                .insert(channel, None, ResourceType::Channel)
                .map_err(GuestError::from)?;
            if let Some(resource_id) = caller.data().entry(slot) {
                registry.record_host_ptr(resource_id, &ptr);
            }
            let handle = GuestUint::try_from(slot).map_err(|_| GuestError::InvalidArgument)?;
            Ok(handle)
        })();

        ready(result)
    }
}

impl<Impl> Contract for ChannelDeleteDriver<Impl>
where
    Impl: ChannelCapability + Clone + Send + 'static,
{
    type Input = GuestUint;
    type Output = ();

    fn to_future(
        &self,
        caller: &mut Caller<'_, InstanceRegistry>,
        channel_id: Self::Input,
    ) -> impl Future<Output = GuestResult<Self::Output>> + 'static {
        let this = self.0.clone();
        let result = (|| -> GuestResult<()> {
            let slot = channel_id as usize;
            let channel = caller
                .data_mut()
                .remove::<Impl::Channel>(slot)
                .ok_or(GuestError::NotFound)?;

            this.delete(channel).map_err(Into::into)?;
            Ok(())
        })();

        ready(result)
    }
}

impl<Impl> Contract for ChannelDrainDriver<Impl>
where
    Impl: ChannelCapability + Clone + Send + 'static,
{
    type Input = u32;
    type Output = ();

    fn to_future(
        &self,
        caller: &mut Caller<'_, InstanceRegistry>,
        channel_id: Self::Input,
    ) -> impl Future<Output = GuestResult<Self::Output>> + 'static {
        let this = self.0.clone();
        let result = (|| -> GuestResult<()> {
            let slot = channel_id as usize;
            caller
                .data()
                .with(slot, |chan| this.drain(chan))
                .ok_or(GuestError::NotFound)?
                .map_err(Into::into)?;
            Ok(())
        })();

        ready(result)
    }
}

impl<Impl> Contract for ChannelDowngradeStrongWriterDriver<Impl>
where
    Impl: ChannelCapability + Send + 'static,
{
    type Input = GuestUint;
    type Output = GuestUint;

    fn to_future(
        &self,
        caller: &mut Caller<'_, InstanceRegistry>,
        input: Self::Input,
    ) -> impl Future<Output = GuestResult<Self::Output>> + 'static {
        match caller
            .data_mut()
            .remove::<Impl::StrongWriter>(input as usize)
            .ok_or(GuestError::NotFound)
            .and_then(|writer| self.0.downgrade_writer(writer).map_err(Into::into))
        {
            Ok(writer) => {
                let result = caller
                    .data_mut()
                    .insert(writer, None, ResourceType::Writer)
                    .map_err(Into::into)
                    .and_then(|idx| {
                        GuestUint::try_from(idx).map_err(|_| GuestError::InvalidArgument)
                    });
                ready(result)
            }
            Err(e) => ready(Err(e)),
        }
    }
}

impl Contract for ChannelExportDriver {
    type Input = GuestUint;
    type Output = GuestResourceId;

    fn to_future(
        &self,
        caller: &mut Caller<'_, InstanceRegistry>,
        handle: Self::Input,
    ) -> impl Future<Output = GuestResult<Self::Output>> + 'static {
        let registry = caller.data().registry_arc();
        let result = caller
            .data()
            .entry(handle as usize)
            .ok_or(GuestError::NotFound)
            .and_then(|rid| registry.share_handle(rid).map_err(GuestError::from));

        ready(result)
    }
}

impl Contract for ChannelAttachDriver {
    type Input = GuestResourceId;
    type Output = GuestUint;

    fn to_future(
        &self,
        caller: &mut Caller<'_, InstanceRegistry>,
        resource_id: Self::Input,
    ) -> impl Future<Output = GuestResult<Self::Output>> + 'static {
        let registry = caller.data().registry_arc();
        let result = registry
            .resolve_shared(resource_id)
            .ok_or(GuestError::NotFound)
            .and_then(|rid| {
                caller
                    .data_mut()
                    .insert_id(rid)
                    .map_err(GuestError::from)
                    .and_then(|slot| {
                        GuestUint::try_from(slot).map_err(|_| GuestError::InvalidArgument)
                    })
            });

        ready(result)
    }
}

impl Contract for ChannelDetachDriver {
    type Input = GuestUint;
    type Output = ();

    fn to_future(
        &self,
        caller: &mut Caller<'_, InstanceRegistry>,
        handle: Self::Input,
    ) -> impl Future<Output = GuestResult<Self::Output>> + 'static {
        let result = caller
            .data_mut()
            .detach_slot(handle as usize)
            .ok_or(GuestError::NotFound)
            .map(|_| ());

        ready(result)
    }
}

impl From<io::Error> for GuestError {
    fn from(err: io::Error) -> Self {
        GuestError::Subsystem(err.to_string())
    }
}

pub fn read_ops<S, W>(strong_cap: S, weak_cap: W) -> ChannelReadOps<S, W>
where
    S: IoCapability + Clone + Send + 'static,
    W: IoCapability + Clone + Send + 'static,
{
    (
        create_reader_op(
            strong_cap.clone(),
            selium_abi::hostcall_contract!(CHANNEL_STRONG_READER_CREATE),
        ),
        create_reader_op(
            weak_cap,
            selium_abi::hostcall_contract!(CHANNEL_WEAK_READER_CREATE),
        ),
        read_op(strong_cap, selium_abi::hostcall_contract!(CHANNEL_READ)),
    )
}

pub fn write_ops<S, W>(strong_cap: S, weak_cap: W) -> ChannelWriteOps<S, W>
where
    S: IoCapability + Clone + Send + 'static,
    W: IoCapability + Clone + Send + 'static,
{
    (
        create_writer_op(
            strong_cap.clone(),
            selium_abi::hostcall_contract!(CHANNEL_STRONG_WRITER_CREATE),
        ),
        create_writer_op(
            weak_cap,
            selium_abi::hostcall_contract!(CHANNEL_WEAK_WRITER_CREATE),
        ),
        write_op(strong_cap, selium_abi::hostcall_contract!(CHANNEL_WRITE)),
    )
}

pub fn writer_downgrade_op<C>(ch_cap: C) -> Arc<Operation<ChannelDowngradeStrongWriterDriver<C>>>
where
    C: ChannelCapability + 'static,
{
    Operation::from_hostcall(
        ChannelDowngradeStrongWriterDriver(ch_cap),
        selium_abi::hostcall_contract!(CHANNEL_WRITER_DOWNGRADE),
    )
}

pub fn lifecycle_ops<C>(cap: C) -> ChannelLifecycleOps<C>
where
    C: ChannelCapability + Clone + 'static,
{
    (
        Operation::from_hostcall(
            ChannelCreateDriver(cap.clone()),
            selium_abi::hostcall_contract!(CHANNEL_CREATE),
        ),
        Operation::from_hostcall(
            ChannelDeleteDriver(cap.clone()),
            selium_abi::hostcall_contract!(CHANNEL_DELETE),
        ),
        Operation::from_hostcall(
            ChannelDrainDriver(cap),
            selium_abi::hostcall_contract!(CHANNEL_DRAIN),
        ),
    )
}

pub fn handoff_ops() -> ChannelHandoffOps {
    (
        Operation::from_hostcall(
            ChannelExportDriver,
            selium_abi::hostcall_contract!(CHANNEL_SHARE),
        ),
        Operation::from_hostcall(
            ChannelAttachDriver,
            selium_abi::hostcall_contract!(CHANNEL_ATTACH),
        ),
        Operation::from_hostcall(
            ChannelDetachDriver,
            selium_abi::hostcall_contract!(CHANNEL_DETACH),
        ),
    )
}
