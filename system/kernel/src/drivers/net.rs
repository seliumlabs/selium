use futures_util::future::BoxFuture;
use std::{future::Future, sync::Arc};

use wasmtime::Caller;

use crate::{
    drivers::io::{self, IoCapability, IoReadDriver, IoWriteDriver},
    guest_data::{GuestError, GuestResult},
    operation::{Contract, Operation},
    registry::{InstanceRegistry, ResourceHandle, ResourceType},
};
use selium_abi::{
    GuestResourceId, NetAccept, NetAcceptReply, NetConnect, NetConnectReply, NetCreateListener,
    NetCreateListenerReply,
};

type NetFuture<'a, T, E> = BoxFuture<'a, Result<T, E>>;
type NetIoFuture<'a, R, W, E> = BoxFuture<'a, Result<(R, W, String), E>>;

pub trait NetCapability {
    type Handle: Send + Unpin;
    type Reader: Send + Unpin;
    type Writer: Send + Unpin;
    type Error: Into<GuestError>;

    /// Creates a new network listener for the implementor's protocol, returning a handle
    /// to the listener.
    fn create(&self, domain: &str, port: u16) -> NetFuture<'_, Self::Handle, Self::Error>;

    /// Connect to a remote listener for the implementor's protocol, returning
    /// bidirectional comms.
    fn connect(
        &self,
        domain: &str,
        port: u16,
    ) -> NetIoFuture<'_, Self::Reader, Self::Writer, Self::Error>;

    /// Accept a new inbound connection for the listener represented by `handle`.
    fn accept(
        &self,
        handle: &Self::Handle,
    ) -> NetIoFuture<'_, Self::Reader, Self::Writer, Self::Error>;
}

/// Driver creating network listeners.
pub struct BindDriver<Impl>(Impl);
/// Driver opening outbound network connections.
pub struct ConnectDriver<Impl>(Impl);
/// Driver responsible for accepting inbound network connections.
pub struct AcceptDriver<Impl>(Impl);

impl<T> NetCapability for Arc<T>
where
    T: NetCapability,
{
    type Handle = T::Handle;
    type Reader = T::Reader;
    type Writer = T::Writer;
    type Error = T::Error;

    fn create(&self, domain: &str, port: u16) -> NetFuture<'_, Self::Handle, Self::Error> {
        self.as_ref().create(domain, port)
    }

    fn connect(
        &self,
        domain: &str,
        port: u16,
    ) -> NetIoFuture<'_, Self::Reader, Self::Writer, Self::Error> {
        self.as_ref().connect(domain, port)
    }

    fn accept(
        &self,
        handle: &Self::Handle,
    ) -> NetIoFuture<'_, Self::Reader, Self::Writer, Self::Error> {
        self.as_ref().accept(handle)
    }
}

impl<Impl> Contract for BindDriver<Impl>
where
    Impl: NetCapability + Clone + Send + 'static,
    Impl::Handle: Send + Unpin,
{
    type Input = NetCreateListener;
    type Output = NetCreateListenerReply;

    fn to_future(
        &self,
        caller: &mut Caller<'_, InstanceRegistry>,
        input: Self::Input,
    ) -> impl Future<Output = GuestResult<Self::Output>> + 'static {
        let inner = self.0.clone();
        let registrar = caller.data().registrar();

        async move {
            let handle = inner
                .create(&input.domain, input.port)
                .await
                .map_err(Into::into)?;

            let slot = registrar
                .insert(handle, None, ResourceType::Network)
                .map_err(GuestError::from)?;
            let handle =
                GuestResourceId::try_from(slot).map_err(|_| GuestError::InvalidArgument)?;

            Ok(NetCreateListenerReply { handle })
        }
    }
}

impl<Impl> Contract for AcceptDriver<Impl>
where
    Impl: NetCapability + Clone + Send + 'static,
    Impl::Handle: Send + Unpin,
    <Impl as NetCapability>::Reader: Send + Unpin,
    <Impl as NetCapability>::Writer: Send + Unpin,
{
    type Input = NetAccept;
    type Output = NetAcceptReply;

    fn to_future(
        &self,
        caller: &mut Caller<'_, InstanceRegistry>,
        input: Self::Input,
    ) -> impl Future<Output = GuestResult<Self::Output>> + 'static {
        let inner = self.0.clone();
        let registrar = caller.data().registrar();
        let registry = caller.data().registry_arc();
        let handle = (|| {
            let slot = usize::try_from(input.handle).map_err(|_| GuestError::InvalidArgument)?;
            caller.data().entry(slot).ok_or(GuestError::NotFound)
        })();

        async move {
            let handle_resource = handle?;

            let (reader, writer, remote_addr) = registry
                .with_async(
                    ResourceHandle::<Impl::Handle>::new(handle_resource),
                    move |handle| Box::pin(async move { inner.accept(handle).await }),
                )
                .await
                .expect("Invalid resource id from InstanceRegistry")
                .map_err(Into::into)?;

            let reader_slot = registrar
                .insert(reader, None, ResourceType::Reader)
                .map_err(GuestError::from)?;
            let writer_slot = registrar
                .insert(writer, None, ResourceType::Writer)
                .map_err(GuestError::from)?;

            let reader =
                GuestResourceId::try_from(reader_slot).map_err(|_| GuestError::InvalidArgument)?;
            let writer =
                GuestResourceId::try_from(writer_slot).map_err(|_| GuestError::InvalidArgument)?;

            Ok(NetAcceptReply {
                reader,
                writer,
                remote_addr,
            })
        }
    }
}

impl<Impl> Contract for ConnectDriver<Impl>
where
    Impl: NetCapability + Clone + Send + 'static,
    <Impl as NetCapability>::Reader: Send + Unpin,
    <Impl as NetCapability>::Writer: Send + Unpin,
{
    type Input = NetConnect;
    type Output = NetConnectReply;

    fn to_future(
        &self,
        caller: &mut Caller<'_, InstanceRegistry>,
        input: Self::Input,
    ) -> impl Future<Output = GuestResult<Self::Output>> + 'static {
        let inner = self.0.clone();
        let registrar = caller.data().registrar();

        async move {
            let (reader, writer, remote_addr) = inner
                .connect(&input.domain, input.port)
                .await
                .map_err(Into::into)?;

            let reader_slot = registrar
                .insert(reader, None, ResourceType::Reader)
                .map_err(GuestError::from)?;
            let writer_slot = registrar
                .insert(writer, None, ResourceType::Writer)
                .map_err(GuestError::from)?;

            let reader =
                GuestResourceId::try_from(reader_slot).map_err(|_| GuestError::InvalidArgument)?;
            let writer =
                GuestResourceId::try_from(writer_slot).map_err(|_| GuestError::InvalidArgument)?;

            Ok(NetConnectReply {
                reader,
                writer,
                remote_addr,
            })
        }
    }
}

pub fn listener_op<C>(cap: C) -> Arc<Operation<BindDriver<C>>>
where
    C: NetCapability + Clone + Send + 'static,
{
    Operation::from_hostcall(BindDriver(cap), selium_abi::hostcall_contract!(NET_BIND))
}

pub fn connect_op<C>(cap: C) -> Arc<Operation<ConnectDriver<C>>>
where
    C: NetCapability + Clone + Send + 'static,
{
    Operation::from_hostcall(
        ConnectDriver(cap),
        selium_abi::hostcall_contract!(NET_CONNECT),
    )
}

/// Host operation for accepting inbound connections on a listener.
pub fn accept_op<C>(cap: C) -> Arc<Operation<AcceptDriver<C>>>
where
    C: NetCapability + Clone + Send + 'static,
{
    Operation::from_hostcall(
        AcceptDriver(cap),
        selium_abi::hostcall_contract!(NET_ACCEPT),
    )
}

pub fn read_op<C>(cap: C) -> Arc<Operation<IoReadDriver<C>>>
where
    C: IoCapability + Clone + Send + 'static,
{
    io::read_op(cap, selium_abi::hostcall_contract!(NET_READ))
}

pub fn write_op<C>(cap: C) -> Arc<Operation<IoWriteDriver<C>>>
where
    C: IoCapability + Clone + Send + 'static,
{
    io::write_op(cap, selium_abi::hostcall_contract!(NET_WRITE))
}
