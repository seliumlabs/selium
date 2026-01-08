use std::{
    collections::{HashMap, HashSet},
    fmt,
    net::{SocketAddr, ToSocketAddrs},
    sync::{Arc, Mutex},
};

use parking_lot::RwLock;
use quinn::{
    ClientConfig, ConnectError, Connection, ConnectionError, Endpoint, RecvStream, SendStream,
    ServerConfig, TransportConfig, VarInt,
    crypto::rustls::{HandshakeData, QuicClientConfig, QuicServerConfig},
    rustls,
};
use rustls::server::danger::ClientCertVerifier;
use rustls::{
    RootCertStore,
    crypto::ring::sign::any_supported_type,
    pki_types::{CertificateDer, PrivateKeyDer},
    server::{ClientHello, ResolvesServerCert, ResolvesServerCertUsingSni, WebPkiClientVerifier},
    sign,
};
use rustls_pki_types::{PrivatePkcs1KeyDer, PrivatePkcs8KeyDer, pem::SliceIter};
use selium_abi::NetProtocol;
use selium_kernel::drivers::net::{TlsClientConfig, TlsServerConfig};
use thiserror::Error;
use webpki_roots::TLS_SERVER_ROOTS;

mod driver;
pub use driver::{ListenerHandle, QuinnDriver};

struct ListenerRegistry {
    listeners: Mutex<HashMap<u16, Arc<Listener>>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ListenerTlsProfile {
    alpn: Vec<Vec<u8>>,
    client_ca_pem: Option<Vec<u8>>,
    require_client_auth: bool,
}

struct Listener {
    #[allow(dead_code)]
    endpoint: Endpoint,
    resolver: Arc<SharedResolver>,
    tls_profile: ListenerTlsProfile,
}

struct SharedResolver {
    resolver: RwLock<ResolvesServerCertUsingSni>,
    domains: RwLock<HashSet<String>>,
}

impl fmt::Debug for SharedResolver {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SharedResolver").finish_non_exhaustive()
    }
}

/// Errors produced by the Quinn-backed listener driver.
#[derive(Error, Debug)]
pub enum QuinnError {
    #[error("listener closed before any connection arrived")]
    ListenerClosed,
    #[error("port out of range")]
    PortRange,
    #[error("failed to resolve {target}: {source}")]
    Resolve {
        target: String,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to build TLS config: {0}")]
    Rustls(rustls::Error),
    #[error("failed to parse certificate chain: {0}")]
    Certificate(String),
    #[error("failed to parse private key: {0}")]
    PrivateKey(String),
    #[error("client certificate provided without private key")]
    ClientKeyMissing,
    #[error("client authentication requires a CA bundle")]
    ClientAuthMissing,
    #[error("failed to configure client authentication: {0}")]
    ClientAuth(String),
    #[error("failed to select QUIC cipher suite")]
    CipherSuite,
    #[error("failed to start listener: {0}")]
    Endpoint(std::io::Error),
    #[error("connect failed: {0}")]
    Connect(ConnectError),
    #[error("connection failed: {0}")]
    Connection(ConnectionError),
    #[error("invalid handshake data")]
    HandshakeData,
    #[error("failed to open bidirectional stream: {0}")]
    Stream(ConnectionError),
    #[error("read failed: {0}")]
    Read(#[source] quinn::ReadError),
    #[error("write failed: {0}")]
    Write(#[source] quinn::WriteError),
    #[error("operation unsupported")]
    Unsupported,
    #[error("TLS configuration does not match existing listener")]
    TlsConfigMismatch,
    #[error("unsupported protocol: {protocol:?}")]
    UnsupportedProtocol { protocol: NetProtocol },
}

impl SharedResolver {
    fn new() -> Self {
        Self {
            resolver: RwLock::new(ResolvesServerCertUsingSni::new()),
            domains: RwLock::new(HashSet::new()),
        }
    }

    fn ensure_domain(
        &self,
        domain: &str,
        certified_key: Arc<sign::CertifiedKey>,
    ) -> Result<(), QuinnError> {
        if self.domains.read().contains(domain) {
            return Ok(());
        }

        let key = (*certified_key).clone();
        {
            let mut resolver = self.resolver.write();
            resolver.add(domain, key).map_err(QuinnError::Rustls)?;
        }
        self.domains.write().insert(domain.to_owned());
        Ok(())
    }
}

impl ResolvesServerCert for SharedResolver {
    fn resolve(&self, client_hello: ClientHello<'_>) -> Option<Arc<sign::CertifiedKey>> {
        self.resolver.read().resolve(client_hello)
    }
}

impl Listener {
    fn new(
        port: u16,
        tls_profile: ListenerTlsProfile,
        client_verifier: Arc<dyn ClientCertVerifier>,
    ) -> Result<Self, QuinnError> {
        let resolver = Arc::new(SharedResolver::new());
        let config = build_server_config(
            Arc::clone(&resolver),
            tls_profile.alpn.clone(),
            client_verifier,
        )?;
        let bind_addr = SocketAddr::from(([0, 0, 0, 0], port));
        let endpoint = Endpoint::server(config, bind_addr).map_err(QuinnError::Endpoint)?;

        Ok(Self {
            endpoint,
            resolver,
            tls_profile,
        })
    }

    fn ensure_domain(
        &self,
        domain: &str,
        certified_key: Arc<sign::CertifiedKey>,
    ) -> Result<(), QuinnError> {
        self.resolver.ensure_domain(domain, certified_key)
    }

    fn tls_profile(&self) -> &ListenerTlsProfile {
        &self.tls_profile
    }

    async fn accept_for_domain(
        &self,
        domain: &str,
    ) -> Result<(RecvStream, SendStream, SocketAddr), QuinnError> {
        loop {
            let connecting = self
                .endpoint
                .accept()
                .await
                .ok_or(QuinnError::ListenerClosed)?
                .accept()
                .map_err(QuinnError::Connection)?;
            let mut connection = connecting.await.map_err(QuinnError::Connection)?;

            match handshake_sni(&mut connection)? {
                Some(name) if name.eq_ignore_ascii_case(domain) => {
                    let (send, recv) = connection.accept_bi().await.map_err(QuinnError::Stream)?;
                    return Ok((recv, send, connection.remote_address()));
                }
                _ => {
                    connection.close(VarInt::from_u32(0), b"unrouted domain");
                }
            }
        }
    }
}

impl ListenerRegistry {
    fn new() -> Self {
        Self {
            listeners: Mutex::new(HashMap::new()),
        }
    }

    fn get_or_try_init(
        &self,
        port: u16,
        tls_profile: ListenerTlsProfile,
        client_verifier: Arc<dyn ClientCertVerifier>,
    ) -> Result<Arc<Listener>, QuinnError> {
        if let Some(listener) = self.listeners.lock().unwrap().get(&port).cloned() {
            if listener.tls_profile() != &tls_profile {
                return Err(QuinnError::TlsConfigMismatch);
            }
            return Ok(listener);
        }

        let listener = Arc::new(Listener::new(port, tls_profile, client_verifier)?);
        self.listeners
            .lock()
            .unwrap()
            .insert(port, Arc::clone(&listener));
        Ok(listener)
    }
}

fn build_server_config(
    resolver: Arc<SharedResolver>,
    alpn: Vec<Vec<u8>>,
    client_verifier: Arc<dyn ClientCertVerifier>,
) -> Result<ServerConfig, QuinnError> {
    let provider = rustls::crypto::ring::default_provider();
    let tls_builder = rustls::ServerConfig::builder_with_provider(provider.into())
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(QuinnError::Rustls)?
        .with_client_cert_verifier(client_verifier);

    let mut tls_config = tls_builder.with_cert_resolver(resolver);
    tls_config.max_early_data_size = u32::MAX;
    tls_config.alpn_protocols = alpn;

    let quic_crypto =
        QuicServerConfig::try_from(Arc::new(tls_config)).map_err(|_| QuinnError::CipherSuite)?;

    let mut server_config = ServerConfig::with_crypto(Arc::new(quic_crypto));
    server_config.transport = Arc::new(TransportConfig::default());
    Ok(server_config)
}

fn build_client_config(
    protocol: NetProtocol,
    tls: Option<&TlsClientConfig>,
) -> Result<ClientConfig, QuinnError> {
    let provider = rustls::crypto::ring::default_provider();
    let tls_builder = rustls::ClientConfig::builder_with_provider(provider.into())
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(QuinnError::Rustls)?;
    if tls.and_then(|cfg| cfg.client_key_pem.as_ref()).is_some()
        && tls.and_then(|cfg| cfg.client_cert_pem.as_ref()).is_none()
    {
        return Err(QuinnError::ClientKeyMissing);
    }
    let roots = match tls.and_then(|cfg| cfg.ca_bundle_pem.as_ref()) {
        Some(pem) => build_root_store(pem)?,
        None => RootCertStore::from_iter(TLS_SERVER_ROOTS.iter().cloned()),
    };
    let mut tls_config = match tls.and_then(|cfg| cfg.client_cert_pem.as_ref()) {
        Some(client_cert_pem) => {
            let client_key_pem = tls
                .and_then(|cfg| cfg.client_key_pem.as_ref())
                .ok_or(QuinnError::ClientKeyMissing)?;
            let cert_chain = parse_certificates(client_cert_pem)?;
            let key = parse_private_key(client_key_pem)?;
            tls_builder
                .with_root_certificates(roots)
                .with_client_auth_cert(cert_chain, key)
                .map_err(QuinnError::Rustls)?
        }
        None => tls_builder
            .with_root_certificates(roots)
            .with_no_client_auth(),
    };
    tls_config.alpn_protocols = resolve_alpn(protocol, tls.and_then(|cfg| cfg.alpn.as_ref()));

    let crypto =
        QuicClientConfig::try_from(Arc::new(tls_config)).map_err(|_| QuinnError::CipherSuite)?;

    let mut client_config = ClientConfig::new(Arc::new(crypto));
    client_config.transport_config(Arc::new(TransportConfig::default()));
    Ok(client_config)
}

#[allow(dead_code)]
fn handshake_sni(connection: &mut Connection) -> Result<Option<String>, QuinnError> {
    let data = connection
        .handshake_data()
        .ok_or(QuinnError::HandshakeData)?
        .downcast::<HandshakeData>()
        .map_err(|_| QuinnError::HandshakeData)?;
    Ok(data.server_name)
}

pub async fn connect_remote(
    protocol: NetProtocol,
    domain: &str,
    port: u16,
    tls: Option<&TlsClientConfig>,
) -> Result<(RecvStream, SendStream, SocketAddr), QuinnError> {
    let target = format!("{domain}:{port}");
    let mut addrs = target
        .to_socket_addrs()
        .map_err(|source| QuinnError::Resolve {
            target: target.clone(),
            source,
        })?;
    let server_addr = addrs.next().ok_or_else(|| QuinnError::Resolve {
        target: target.clone(),
        source: std::io::Error::new(
            std::io::ErrorKind::AddrNotAvailable,
            "no socket addresses returned",
        ),
    })?;

    let bind_addr = match server_addr {
        SocketAddr::V4(_) => SocketAddr::from(([0, 0, 0, 0], 0)),
        SocketAddr::V6(_) => SocketAddr::from(([0, 0, 0, 0, 0, 0, 0, 0], 0)),
    };
    let mut endpoint = Endpoint::client(bind_addr).map_err(QuinnError::Endpoint)?;
    endpoint.set_default_client_config(build_client_config(protocol, tls)?);

    let connecting = endpoint
        .connect(server_addr, domain)
        .map_err(QuinnError::Connect)?;
    let connection = connecting.await.map_err(QuinnError::Connection)?;
    let (send, recv) = connection.open_bi().await.map_err(QuinnError::Stream)?;
    Ok((recv, send, bind_addr))
}

pub(crate) fn certified_key_from_config(
    config: &TlsServerConfig,
) -> Result<Arc<sign::CertifiedKey>, QuinnError> {
    let certificates = parse_certificates(&config.cert_chain_pem)?;
    let private_key = parse_private_key(&config.private_key_pem)?;
    let signing_key = any_supported_type(&private_key).map_err(QuinnError::Rustls)?;
    let mut certified_key = sign::CertifiedKey::new(certificates, signing_key);
    certified_key.ocsp = None;
    Ok(Arc::new(certified_key))
}

pub(crate) fn build_client_verifier(
    client_ca_pem: Option<&Vec<u8>>,
    require_client_auth: bool,
) -> Result<Arc<dyn ClientCertVerifier>, QuinnError> {
    match (client_ca_pem, require_client_auth) {
        (None, false) => Ok(WebPkiClientVerifier::no_client_auth()),
        (None, true) => Err(QuinnError::ClientAuthMissing),
        (Some(pem), require_client_auth) => {
            let roots = build_root_store(pem)?;
            let builder = WebPkiClientVerifier::builder(Arc::new(roots));
            let builder = if require_client_auth {
                builder
            } else {
                builder.allow_unauthenticated()
            };
            builder
                .build()
                .map_err(|err| QuinnError::ClientAuth(err.to_string()))
        }
    }
}

pub(crate) fn resolve_alpn(
    protocol: NetProtocol,
    override_alpn: Option<&Vec<String>>,
) -> Vec<Vec<u8>> {
    match override_alpn {
        Some(values) => values
            .iter()
            .map(|value| value.as_bytes().to_vec())
            .collect(),
        None => match protocol {
            NetProtocol::Https => vec![b"h2".to_vec()],
            _ => Vec::new(),
        },
    }
}

fn build_root_store(pem: &[u8]) -> Result<RootCertStore, QuinnError> {
    let mut store = RootCertStore::empty();
    for cert in parse_certificates(pem)? {
        store
            .add(cert)
            .map_err(|err| QuinnError::Certificate(err.to_string()))?;
    }
    Ok(store)
}

fn parse_certificates(bytes: &[u8]) -> Result<Vec<CertificateDer<'static>>, QuinnError> {
    let parsed = SliceIter::new(bytes)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|err| QuinnError::Certificate(err.to_string()))?;
    if !parsed.is_empty() {
        return Ok(parsed);
    }

    Ok(vec![CertificateDer::from(bytes.to_vec())])
}

fn parse_private_key(bytes: &[u8]) -> Result<PrivateKeyDer<'static>, QuinnError> {
    let pkcs8 = SliceIter::new(bytes)
        .collect::<Result<Vec<PrivatePkcs8KeyDer>, _>>()
        .map_err(|err| QuinnError::PrivateKey(err.to_string()))?;
    if let Some(key) = pkcs8.into_iter().next() {
        return Ok(key.into());
    }

    let rsa = SliceIter::new(bytes)
        .collect::<Result<Vec<PrivatePkcs1KeyDer>, _>>()
        .map_err(|err| QuinnError::PrivateKey(err.to_string()))?;
    if let Some(key) = rsa.into_iter().next() {
        return Ok(key.into());
    }

    PrivateKeyDer::try_from(bytes.to_vec())
        .map_err(|_| QuinnError::PrivateKey("no private key found in provided material".into()))
}
