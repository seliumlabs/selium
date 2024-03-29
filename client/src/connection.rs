use crate::utils::net::get_socket_addrs;
use quinn::{ClientConfig, Connection, Endpoint, TransportConfig};
use rustls::{Certificate, PrivateKey, RootCertStore};
use selium_std::errors::{ParseEndpointAddressError, QuicError, Result};
use std::sync::Arc;
use std::{net::SocketAddr, time::Duration};
use tokio::sync::Mutex;

const ALPN_QUIC_HTTP: &[&[u8]] = &[b"hq-29"];
const ENDPOINT_ADDRESS: &str = "[::]:0";

pub type SharedConnection = Arc<Mutex<ClientConnection>>;

#[derive(Debug, Clone)]
pub struct ConnectionOptions {
    certs: Vec<Certificate>,
    key: PrivateKey,
    root_store: RootCertStore,
    keep_alive: u64,
}

impl ConnectionOptions {
    pub fn new(
        certs: &[Certificate],
        key: PrivateKey,
        root_store: RootCertStore,
        keep_alive: u64,
    ) -> Self {
        Self {
            certs: certs.to_vec(),
            key,
            root_store,
            keep_alive,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ClientConnection {
    addr: SocketAddr,
    connection: Connection,
    client_config: ClientConfig,
}

impl ClientConnection {
    pub async fn connect(addr: &str, options: ConnectionOptions) -> Result<Self> {
        let client_config = configure_client(options);
        let addr = get_socket_addrs(addr)?;
        let connection = connect_to_endpoint(addr, client_config.clone()).await?;

        Ok(Self {
            addr,
            connection,
            client_config,
        })
    }

    pub fn conn(&self) -> &Connection {
        &self.connection
    }

    pub async fn reconnect(&mut self) -> Result<()> {
        if self.connection.close_reason().is_some() {
            let connection = connect_to_endpoint(self.addr, self.client_config.clone()).await?;
            self.connection = connection;
        }

        Ok(())
    }
}

fn configure_client(options: ConnectionOptions) -> ClientConfig {
    let mut crypto = rustls::ClientConfig::builder()
        .with_safe_defaults()
        .with_root_certificates(options.root_store)
        .with_client_auth_cert(options.certs, options.key)
        .unwrap();

    crypto.alpn_protocols = ALPN_QUIC_HTTP.iter().map(|&x| x.into()).collect();

    let mut config = ClientConfig::new(Arc::new(crypto));
    let mut transport_config = TransportConfig::default();
    let keep_alive = Duration::from_millis(options.keep_alive);

    transport_config.keep_alive_interval(Some(keep_alive));
    config.transport_config(Arc::new(transport_config));

    config
}

async fn connect_to_endpoint(addr: SocketAddr, config: ClientConfig) -> Result<Connection> {
    let endpoint_addr = ENDPOINT_ADDRESS
        .parse::<SocketAddr>()
        .map_err(ParseEndpointAddressError::InvalidAddress)?;

    let mut endpoint = Endpoint::client(endpoint_addr)?;
    endpoint.set_default_client_config(config);
    let connection = endpoint
        .connect(addr, "localhost")
        .map_err(QuicError::ConnectError)?
        .await
        .map_err(QuicError::ConnectionError)?;

    Ok(connection)
}
