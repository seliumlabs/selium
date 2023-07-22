use super::net::get_socket_addrs;
use crate::protocol::MessageCodec;
use crate::BiStream;
use anyhow::Result;
use quinn::{ClientConfig, Connection, Endpoint, TransportConfig};
use rustls::RootCertStore;
use std::sync::Arc;
use std::{net::SocketAddr, time::Duration};
use tokio_util::codec::{FramedRead, FramedWrite};

pub const ALPN_QUIC_HTTP: &[&[u8]] = &[b"hq-29"];

pub fn configure_client(root_store: &RootCertStore, keep_alive: u64) -> Result<ClientConfig> {
    let mut crypto = rustls::ClientConfig::builder()
        .with_safe_defaults()
        .with_root_certificates(root_store.to_owned())
        .with_no_client_auth();

    crypto.alpn_protocols = ALPN_QUIC_HTTP.iter().map(|&x| x.into()).collect();

    let mut config = ClientConfig::new(Arc::new(crypto));
    let mut transport_config = TransportConfig::default();
    let keep_alive = Duration::from_millis(keep_alive);

    transport_config.keep_alive_interval(Some(keep_alive));
    config.transport_config(Arc::new(transport_config));

    Ok(config)
}

pub async fn connect_to_endpoint(config: ClientConfig, addr: SocketAddr) -> Result<Connection> {
    let mut endpoint = Endpoint::client("[::]:0".parse()?)?;
    endpoint.set_default_client_config(config);

    let connection = endpoint.connect(addr, "localhost")?.await?;

    Ok(connection)
}

pub async fn get_client_stream(connection: Connection) -> Result<BiStream> {
    let (write, read) = connection.open_bi().await?;

    let write_stream = FramedWrite::new(write, MessageCodec::new());
    let read_stream = FramedRead::new(read, MessageCodec::new());

    Ok(BiStream(write_stream, read_stream))
}

pub async fn establish_connection(
    host: &str,
    root_store: &RootCertStore,
    keep_alive: u64,
) -> Result<BiStream> {
    let addr = get_socket_addrs(host)?;

    let config = configure_client(root_store, keep_alive)?;
    let connection = connect_to_endpoint(config, addr).await?;
    let streams = get_client_stream(connection).await?;

    Ok(streams)
}
