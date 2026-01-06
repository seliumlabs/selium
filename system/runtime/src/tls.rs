use std::{future::Future, sync::Arc};

use selium_abi::{
    GuestResourceId, NetTlsClientConfig, NetTlsConfigReply, NetTlsServerConfig, TlsClientBundle,
    TlsServerBundle,
};
use selium_kernel::{
    drivers::net::{TlsClientConfig, TlsServerConfig},
    guest_data::{GuestError, GuestResult},
    operation::{Contract, Operation},
    registry::{InstanceRegistry, ResourceType},
};
use wasmtime::Caller;

const TLS_BUNDLE_MAX_BYTES: usize = 1024 * 1024;

/// Build hostcall operations for TLS configuration creation.
pub fn operations() -> (
    Arc<Operation<TlsServerConfigCreateDriver>>,
    Arc<Operation<TlsClientConfigCreateDriver>>,
) {
    (
        Operation::from_hostcall(
            TlsServerConfigCreateDriver,
            selium_abi::hostcall_contract!(NET_TLS_SERVER_CONFIG_CREATE),
        ),
        Operation::from_hostcall(
            TlsClientConfigCreateDriver,
            selium_abi::hostcall_contract!(NET_TLS_CLIENT_CONFIG_CREATE),
        ),
    )
}

/// Hostcall driver that registers server-side TLS configurations.
pub struct TlsServerConfigCreateDriver;

impl Contract for TlsServerConfigCreateDriver {
    type Input = NetTlsServerConfig;
    type Output = NetTlsConfigReply;

    fn to_future(
        &self,
        caller: &mut Caller<'_, InstanceRegistry>,
        input: Self::Input,
    ) -> impl Future<Output = GuestResult<Self::Output>> + Send + 'static {
        let registrar = caller.data().registrar();
        let NetTlsServerConfig { bundle } = input;

        async move {
            let config = parse_server_bundle(bundle)?;
            let slot = registrar
                .insert(Arc::new(config), None, ResourceType::Network)
                .map_err(GuestError::from)?;
            let handle =
                GuestResourceId::try_from(slot).map_err(|_| GuestError::InvalidArgument)?;
            Ok(NetTlsConfigReply { handle })
        }
    }
}

/// Hostcall driver that registers client-side TLS configurations.
pub struct TlsClientConfigCreateDriver;

impl Contract for TlsClientConfigCreateDriver {
    type Input = NetTlsClientConfig;
    type Output = NetTlsConfigReply;

    fn to_future(
        &self,
        caller: &mut Caller<'_, InstanceRegistry>,
        input: Self::Input,
    ) -> impl Future<Output = GuestResult<Self::Output>> + Send + 'static {
        let registrar = caller.data().registrar();
        let NetTlsClientConfig { bundle } = input;

        async move {
            let config = parse_client_bundle(bundle)?;
            let slot = registrar
                .insert(Arc::new(config), None, ResourceType::Network)
                .map_err(GuestError::from)?;
            let handle =
                GuestResourceId::try_from(slot).map_err(|_| GuestError::InvalidArgument)?;
            Ok(NetTlsConfigReply { handle })
        }
    }
}

fn parse_server_bundle(bundle: TlsServerBundle) -> Result<TlsServerConfig, GuestError> {
    if server_bundle_size_bytes(&bundle) > TLS_BUNDLE_MAX_BYTES {
        return Err(GuestError::InvalidArgument);
    }
    let TlsServerBundle {
        cert_chain_pem,
        private_key_pem,
        client_ca_pem,
        alpn,
        require_client_auth,
    } = bundle;
    if cert_chain_pem.is_empty() {
        return Err(GuestError::InvalidArgument);
    }
    if private_key_pem.is_empty() {
        return Err(GuestError::InvalidArgument);
    }
    let client_ca_pem = client_ca_pem.filter(|value| !value.is_empty());
    if require_client_auth && client_ca_pem.is_none() {
        return Err(GuestError::InvalidArgument);
    }
    let alpn = alpn
        .map(|values| {
            if values.iter().any(|value| value.is_empty()) {
                return Err(GuestError::InvalidArgument);
            }
            Ok(values)
        })
        .transpose()?;

    Ok(TlsServerConfig {
        cert_chain_pem,
        private_key_pem,
        client_ca_pem,
        alpn,
        require_client_auth,
    })
}

fn parse_client_bundle(bundle: TlsClientBundle) -> Result<TlsClientConfig, GuestError> {
    if client_bundle_size_bytes(&bundle) > TLS_BUNDLE_MAX_BYTES {
        return Err(GuestError::InvalidArgument);
    }
    let TlsClientBundle {
        ca_bundle_pem,
        client_cert_pem,
        client_key_pem,
        alpn,
    } = bundle;
    let ca_bundle_pem = ca_bundle_pem.filter(|value| !value.is_empty());
    let client_cert_pem = client_cert_pem.filter(|value| !value.is_empty());
    let client_key_pem = client_key_pem.filter(|value| !value.is_empty());
    if client_cert_pem.is_some() != client_key_pem.is_some() {
        return Err(GuestError::InvalidArgument);
    }
    let alpn = alpn
        .map(|values| {
            if values.iter().any(|value| value.is_empty()) {
                return Err(GuestError::InvalidArgument);
            }
            Ok(values)
        })
        .transpose()?;

    Ok(TlsClientConfig {
        ca_bundle_pem,
        client_cert_pem,
        client_key_pem,
        alpn,
    })
}

fn server_bundle_size_bytes(bundle: &TlsServerBundle) -> usize {
    let mut total = bundle.cert_chain_pem.len();
    total = total.saturating_add(bundle.private_key_pem.len());
    if let Some(value) = &bundle.client_ca_pem {
        total = total.saturating_add(value.len());
    }
    if let Some(values) = &bundle.alpn {
        for value in values {
            total = total.saturating_add(value.len());
        }
    }
    if bundle.require_client_auth {
        total = total.saturating_add(1);
    }
    total
}

fn client_bundle_size_bytes(bundle: &TlsClientBundle) -> usize {
    let mut total: usize = 0;
    if let Some(value) = &bundle.ca_bundle_pem {
        total = total.saturating_add(value.len());
    }
    if let Some(value) = &bundle.client_cert_pem {
        total = total.saturating_add(value.len());
    }
    if let Some(value) = &bundle.client_key_pem {
        total = total.saturating_add(value.len());
    }
    if let Some(values) = &bundle.alpn {
        for value in values {
            total = total.saturating_add(value.len());
        }
    }
    total
}
