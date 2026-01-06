use std::sync::Arc;

use futures_util::future::BoxFuture;
use quinn::rustls::sign;
use quinn::{RecvStream, SendStream};
use selium_abi::{IoFrame, NetProtocol};
use selium_kernel::{
    drivers::{
        io::IoCapability,
        net::{NetCapability, TlsClientConfig, TlsServerConfig},
    },
    guest_data::GuestError,
};
use tracing::debug;

use crate::{
    Listener, ListenerRegistry, ListenerTlsProfile, QuinnError, build_client_verifier,
    certified_key_from_config, connect_remote, resolve_alpn,
};

/// Handle for a bound QUIC listener.
#[derive(Clone)]
pub struct ListenerHandle {
    listener: Arc<Listener>,
    domain: String,
}

impl ListenerHandle {
    pub(crate) fn new(listener: Arc<Listener>, domain: String) -> Self {
        Self { listener, domain }
    }

    pub fn domain(&self) -> &str {
        &self.domain
    }
}

/// Quinn-backed network driver.
pub struct QuinnDriver {
    registry: Arc<ListenerRegistry>,
    default_certified_key: Arc<sign::CertifiedKey>,
}

impl QuinnDriver {
    /// Create a new driver instance with an already validated certificate and private key.
    pub fn new(certified_key: Arc<sign::CertifiedKey>) -> Arc<Self> {
        Arc::new(Self {
            registry: Arc::new(ListenerRegistry::new()),
            default_certified_key: certified_key,
        })
    }
}

impl NetCapability for QuinnDriver {
    type Handle = ListenerHandle;
    type Reader = RecvStream;
    type Writer = SendStream;
    type Error = QuinnError;

    fn create(
        &self,
        protocol: NetProtocol,
        domain: &str,
        port: u16,
        tls: Option<Arc<TlsServerConfig>>,
    ) -> BoxFuture<'_, Result<Self::Handle, Self::Error>> {
        let domain = domain.to_owned();
        let registry = Arc::clone(&self.registry);
        let default_certified_key = Arc::clone(&self.default_certified_key);

        Box::pin(async move {
            ensure_quic(protocol)?;
            let (certified_key, tls_profile, client_verifier) = match tls.as_ref() {
                Some(config) => {
                    let alpn = resolve_alpn(protocol, config.alpn.as_ref());
                    let profile = ListenerTlsProfile {
                        alpn: alpn.clone(),
                        client_ca_pem: config.client_ca_pem.clone(),
                        require_client_auth: config.require_client_auth,
                    };
                    let verifier = build_client_verifier(
                        config.client_ca_pem.as_ref(),
                        config.require_client_auth,
                    )?;
                    let key = certified_key_from_config(config)?;
                    (key, profile, verifier)
                }
                None => {
                    let alpn = resolve_alpn(protocol, None);
                    let profile = ListenerTlsProfile {
                        alpn,
                        client_ca_pem: None,
                        require_client_auth: false,
                    };
                    let verifier = build_client_verifier(None, false)?;
                    (default_certified_key, profile, verifier)
                }
            };

            let listener = registry.get_or_try_init(port, tls_profile, client_verifier)?;
            listener.ensure_domain(&domain, certified_key)?;
            Ok(ListenerHandle::new(listener, domain))
        })
    }

    fn connect(
        &self,
        protocol: NetProtocol,
        domain: &str,
        port: u16,
        tls: Option<Arc<TlsClientConfig>>,
    ) -> BoxFuture<'_, Result<(Self::Reader, Self::Writer, String), Self::Error>> {
        let domain = domain.to_owned();

        Box::pin(async move {
            ensure_quic(protocol)?;
            let tls = tls.as_deref();
            let (reader, writer, remote_addr) =
                connect_remote(protocol, &domain, port, tls).await?;
            Ok((reader, writer, format!("{remote_addr}")))
        })
    }

    fn accept(
        &self,
        handle: &Self::Handle,
    ) -> BoxFuture<'_, Result<(Self::Reader, Self::Writer, String), Self::Error>> {
        let listener = Arc::clone(&handle.listener);
        let domain = handle.domain.clone();

        Box::pin(async move {
            let (reader, writer, remote_addr) = listener.accept_for_domain(&domain).await?;
            Ok((reader, writer, format!("{remote_addr}")))
        })
    }
}

impl IoCapability for QuinnDriver {
    type Handle = ();
    type Reader = RecvStream;
    type Writer = SendStream;
    type Error = QuinnError;

    fn new_writer(&self, _handle: &Self::Handle) -> Result<Self::Writer, Self::Error> {
        Err(QuinnError::Unsupported)
    }

    fn new_reader(&self, _handle: &Self::Handle) -> Result<Self::Reader, Self::Error> {
        Err(QuinnError::Unsupported)
    }

    async fn read(&self, reader: &mut Self::Reader, len: usize) -> Result<IoFrame, Self::Error> {
        let mut buf = vec![0u8; len];
        let read = reader.read(&mut buf).await.map_err(QuinnError::Read)?;
        let bytes_read = read.unwrap_or(0);
        buf.truncate(bytes_read);
        debug!(bytes_read, requested = len, "quinn read");
        Ok(IoFrame {
            writer_id: 0,
            payload: buf,
        })
    }

    async fn write(&self, writer: &mut Self::Writer, bytes: &[u8]) -> Result<(), Self::Error> {
        debug!(bytes = bytes.len(), "quinn write");
        writer.write_all(bytes).await.map_err(QuinnError::Write)?;
        Ok(())
    }
}

impl From<QuinnError> for GuestError {
    fn from(value: QuinnError) -> Self {
        match value {
            QuinnError::PortRange => GuestError::InvalidArgument,
            QuinnError::Resolve { .. } => GuestError::InvalidArgument,
            QuinnError::Certificate(_) => GuestError::InvalidArgument,
            QuinnError::PrivateKey(_) => GuestError::InvalidArgument,
            QuinnError::ClientKeyMissing => GuestError::InvalidArgument,
            QuinnError::ClientAuthMissing => GuestError::InvalidArgument,
            QuinnError::ClientAuth(_) => GuestError::InvalidArgument,
            QuinnError::TlsConfigMismatch => GuestError::InvalidArgument,
            QuinnError::UnsupportedProtocol { .. } => GuestError::InvalidArgument,
            _ => GuestError::Subsystem(value.to_string()),
        }
    }
}

fn ensure_quic(protocol: NetProtocol) -> Result<(), QuinnError> {
    if protocol == NetProtocol::Quic {
        Ok(())
    } else {
        Err(QuinnError::UnsupportedProtocol { protocol })
    }
}
