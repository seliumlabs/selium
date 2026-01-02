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
use rustls::{
    server::{ClientHello, ResolvesServerCert, ResolvesServerCertUsingSni},
    sign,
};
use thiserror::Error;
use webpki_roots::TLS_SERVER_ROOTS;

mod driver;
pub use driver::{ListenerHandle, QuinnDriver};

struct ListenerRegistry {
    listeners: Mutex<HashMap<u16, Arc<Listener>>>,
    certified_key: Arc<sign::CertifiedKey>,
}

struct Listener {
    #[allow(dead_code)]
    endpoint: Endpoint,
    resolver: Arc<SharedResolver>,
}

struct SharedResolver {
    resolver: RwLock<ResolvesServerCertUsingSni>,
    domains: RwLock<HashSet<String>>,
    certified_key: Arc<sign::CertifiedKey>,
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
}

impl SharedResolver {
    fn new(certified_key: Arc<sign::CertifiedKey>) -> Self {
        Self {
            resolver: RwLock::new(ResolvesServerCertUsingSni::new()),
            domains: RwLock::new(HashSet::new()),
            certified_key,
        }
    }

    fn ensure_domain(&self, domain: &str) -> Result<(), QuinnError> {
        if self.domains.read().contains(domain) {
            return Ok(());
        }

        let key = (*self.certified_key).clone();
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
    fn new(port: u16, certified_key: Arc<sign::CertifiedKey>) -> Result<Self, QuinnError> {
        let resolver = Arc::new(SharedResolver::new(certified_key));
        let config = build_server_config(Arc::clone(&resolver))?;
        let bind_addr = SocketAddr::from(([0, 0, 0, 0], port));
        let endpoint = Endpoint::server(config, bind_addr).map_err(QuinnError::Endpoint)?;

        Ok(Self { endpoint, resolver })
    }

    fn ensure_domain(&self, domain: &str) -> Result<(), QuinnError> {
        self.resolver.ensure_domain(domain)
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
    fn new(certified_key: Arc<sign::CertifiedKey>) -> Self {
        Self {
            listeners: Mutex::new(HashMap::new()),
            certified_key,
        }
    }

    fn get_or_try_init(&self, port: u16) -> Result<Arc<Listener>, QuinnError> {
        if let Some(listener) = self.listeners.lock().unwrap().get(&port).cloned() {
            return Ok(listener);
        }

        let listener = Arc::new(Listener::new(port, Arc::clone(&self.certified_key))?);
        self.listeners
            .lock()
            .unwrap()
            .insert(port, Arc::clone(&listener));
        Ok(listener)
    }
}

fn client_config() -> Result<ClientConfig, QuinnError> {
    build_client_config()
}

fn build_server_config(resolver: Arc<SharedResolver>) -> Result<ServerConfig, QuinnError> {
    let provider = rustls::crypto::ring::default_provider();
    let tls_builder = rustls::ServerConfig::builder_with_provider(provider.into())
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(QuinnError::Rustls)?
        .with_no_client_auth();

    let mut tls_config = tls_builder.with_cert_resolver(resolver);
    tls_config.max_early_data_size = u32::MAX;

    let quic_crypto =
        QuicServerConfig::try_from(Arc::new(tls_config)).map_err(|_| QuinnError::CipherSuite)?;

    let mut server_config = ServerConfig::with_crypto(Arc::new(quic_crypto));
    server_config.transport = Arc::new(TransportConfig::default());
    Ok(server_config)
}

fn build_client_config() -> Result<ClientConfig, QuinnError> {
    let provider = rustls::crypto::ring::default_provider();
    let tls_builder = rustls::ClientConfig::builder_with_provider(provider.into())
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(QuinnError::Rustls)?;
    let roots = rustls::RootCertStore::from_iter(TLS_SERVER_ROOTS.iter().cloned());
    let tls_config = tls_builder
        .with_root_certificates(roots)
        .with_no_client_auth();

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
    domain: &str,
    port: u16,
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
    endpoint.set_default_client_config(client_config()?);

    let connecting = endpoint
        .connect(server_addr, domain)
        .map_err(QuinnError::Connect)?;
    let connection = connecting.await.map_err(QuinnError::Connection)?;
    let (send, recv) = connection.open_bi().await.map_err(QuinnError::Stream)?;
    Ok((recv, send, bind_addr))
}
