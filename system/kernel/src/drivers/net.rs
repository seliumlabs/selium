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
    NetCreateListenerReply, NetProtocol, hostcalls::Hostcall,
};

type NetFuture<'a, T, E> = BoxFuture<'a, Result<T, E>>;
type NetIoFuture<'a, R, W, E> = BoxFuture<'a, Result<(R, W, String), E>>;

pub trait NetCapability {
    type Handle: Send + Unpin;
    type Reader: Send + Unpin;
    type Writer: Send + Unpin;
    type Error: Into<GuestError>;

    /// Creates a new network listener for the selected protocol, returning a handle
    /// to the listener.
    fn create(
        &self,
        protocol: NetProtocol,
        domain: &str,
        port: u16,
        tls: Option<Arc<TlsServerConfig>>,
    ) -> NetFuture<'_, Self::Handle, Self::Error>;

    /// Connect to a remote listener for the selected protocol, returning
    /// bidirectional comms.
    fn connect(
        &self,
        protocol: NetProtocol,
        domain: &str,
        port: u16,
        tls: Option<Arc<TlsClientConfig>>,
    ) -> NetIoFuture<'_, Self::Reader, Self::Writer, Self::Error>;

    /// Accept a new inbound connection for the listener represented by `handle`.
    fn accept(
        &self,
        handle: &Self::Handle,
    ) -> NetIoFuture<'_, Self::Reader, Self::Writer, Self::Error>;
}

/// TLS configuration supplied for server listeners.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TlsServerConfig {
    /// PEM-encoded certificate chain presented by the server.
    pub cert_chain_pem: Vec<u8>,
    /// PEM-encoded private key for the certificate chain.
    pub private_key_pem: Vec<u8>,
    /// PEM-encoded CA bundle used to verify client certificates.
    pub client_ca_pem: Option<Vec<u8>>,
    /// Optional ALPN protocol list override.
    pub alpn: Option<Vec<String>>,
    /// Require client authentication when true.
    pub require_client_auth: bool,
}

/// TLS configuration supplied for client connections.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TlsClientConfig {
    /// PEM-encoded CA bundle used to verify servers.
    pub ca_bundle_pem: Option<Vec<u8>>,
    /// PEM-encoded client certificate chain.
    pub client_cert_pem: Option<Vec<u8>>,
    /// PEM-encoded private key for the client certificate.
    pub client_key_pem: Option<Vec<u8>>,
    /// Optional ALPN protocol list override.
    pub alpn: Option<Vec<String>>,
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

    fn create(
        &self,
        protocol: NetProtocol,
        domain: &str,
        port: u16,
        tls: Option<Arc<TlsServerConfig>>,
    ) -> NetFuture<'_, Self::Handle, Self::Error> {
        self.as_ref().create(protocol, domain, port, tls)
    }

    fn connect(
        &self,
        protocol: NetProtocol,
        domain: &str,
        port: u16,
        tls: Option<Arc<TlsClientConfig>>,
    ) -> NetIoFuture<'_, Self::Reader, Self::Writer, Self::Error> {
        self.as_ref().connect(protocol, domain, port, tls)
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
        let registry = caller.data().registry_arc();
        let NetCreateListener {
            protocol,
            domain,
            port,
            tls,
        } = input;
        let tls = resolve_tls_server_config(caller.data(), &registry, protocol, tls);

        async move {
            let handle = inner
                .create(protocol, &domain, port, tls?)
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
        let registry = caller.data().registry_arc();
        let NetConnect {
            protocol,
            domain,
            port,
            tls,
        } = input;
        let tls = resolve_tls_client_config(caller.data(), &registry, protocol, tls);

        async move {
            let (reader, writer, remote_addr) = inner
                .connect(protocol, &domain, port, tls?)
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

pub fn listener_op<C>(cap: C, protocol: NetProtocol) -> Arc<Operation<BindDriver<C>>>
where
    C: NetCapability + Clone + Send + 'static,
{
    let hostcall = hostcall_for_protocol(
        protocol,
        selium_abi::hostcall_contract!(NET_QUIC_BIND),
        selium_abi::hostcall_contract!(NET_HTTP_BIND),
    );
    Operation::from_hostcall(BindDriver(cap), hostcall)
}

pub fn connect_op<C>(cap: C, protocol: NetProtocol) -> Arc<Operation<ConnectDriver<C>>>
where
    C: NetCapability + Clone + Send + 'static,
{
    let hostcall = hostcall_for_protocol(
        protocol,
        selium_abi::hostcall_contract!(NET_QUIC_CONNECT),
        selium_abi::hostcall_contract!(NET_HTTP_CONNECT),
    );
    Operation::from_hostcall(ConnectDriver(cap), hostcall)
}

/// Host operation for accepting inbound connections on a listener.
pub fn accept_op<C>(cap: C, protocol: NetProtocol) -> Arc<Operation<AcceptDriver<C>>>
where
    C: NetCapability + Clone + Send + 'static,
{
    let hostcall = hostcall_for_protocol(
        protocol,
        selium_abi::hostcall_contract!(NET_QUIC_ACCEPT),
        selium_abi::hostcall_contract!(NET_HTTP_ACCEPT),
    );
    Operation::from_hostcall(AcceptDriver(cap), hostcall)
}

pub fn read_op<C>(cap: C, protocol: NetProtocol) -> Arc<Operation<IoReadDriver<C>>>
where
    C: IoCapability + Clone + Send + 'static,
{
    let hostcall = hostcall_for_protocol(
        protocol,
        selium_abi::hostcall_contract!(NET_QUIC_READ),
        selium_abi::hostcall_contract!(NET_HTTP_READ),
    );
    io::read_op(cap, hostcall)
}

pub fn write_op<C>(cap: C, protocol: NetProtocol) -> Arc<Operation<IoWriteDriver<C>>>
where
    C: IoCapability + Clone + Send + 'static,
{
    let hostcall = hostcall_for_protocol(
        protocol,
        selium_abi::hostcall_contract!(NET_QUIC_WRITE),
        selium_abi::hostcall_contract!(NET_HTTP_WRITE),
    );
    io::write_op(cap, hostcall)
}

fn resolve_tls_server_config(
    instance: &InstanceRegistry,
    registry: &Arc<crate::registry::Registry>,
    protocol: NetProtocol,
    handle: Option<GuestResourceId>,
) -> GuestResult<Option<Arc<TlsServerConfig>>> {
    let Some(handle) = handle else {
        return Ok(None);
    };
    if matches!(protocol, NetProtocol::Http) {
        return Err(GuestError::InvalidArgument);
    }
    let slot = usize::try_from(handle).map_err(|_| GuestError::InvalidArgument)?;
    let resource_id = instance.entry(slot).ok_or(GuestError::NotFound)?;
    let config = registry
        .with(
            ResourceHandle::<Arc<TlsServerConfig>>::new(resource_id),
            |config| Arc::clone(config),
        )
        .ok_or(GuestError::NotFound)?;
    Ok(Some(config))
}

fn resolve_tls_client_config(
    instance: &InstanceRegistry,
    registry: &Arc<crate::registry::Registry>,
    protocol: NetProtocol,
    handle: Option<GuestResourceId>,
) -> GuestResult<Option<Arc<TlsClientConfig>>> {
    let Some(handle) = handle else {
        return Ok(None);
    };
    if matches!(protocol, NetProtocol::Http) {
        return Err(GuestError::InvalidArgument);
    }
    let slot = usize::try_from(handle).map_err(|_| GuestError::InvalidArgument)?;
    let resource_id = instance.entry(slot).ok_or(GuestError::NotFound)?;
    let config = registry
        .with(
            ResourceHandle::<Arc<TlsClientConfig>>::new(resource_id),
            |config| Arc::clone(config),
        )
        .ok_or(GuestError::NotFound)?;
    Ok(Some(config))
}

fn hostcall_for_protocol<I, O>(
    protocol: NetProtocol,
    quic: &'static Hostcall<I, O>,
    http: &'static Hostcall<I, O>,
) -> &'static Hostcall<I, O> {
    match protocol {
        NetProtocol::Quic => quic,
        NetProtocol::Http | NetProtocol::Https => http,
    }
}
