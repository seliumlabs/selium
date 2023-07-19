use crate::aliases::Streams;
use crate::protocol::MessageCodec;
use anyhow::Result;
use quinn::{ClientConfig, Connection, Endpoint};
use rustls::RootCertStore;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio_util::codec::{FramedRead, FramedWrite};

pub const ALPN_QUIC_HTTP: &[&[u8]] = &[b"hq-29"];

pub fn configure_client(root_store: &RootCertStore) -> Result<ClientConfig> {
    let mut crypto = rustls::ClientConfig::builder()
        .with_safe_defaults()
        .with_root_certificates(root_store.to_owned())
        .with_no_client_auth();

    crypto.alpn_protocols = ALPN_QUIC_HTTP.iter().map(|&x| x.into()).collect();
    let config = ClientConfig::new(Arc::new(crypto));

    Ok(config)
}

pub async fn get_client_connection(config: ClientConfig, addr: SocketAddr) -> Result<Connection> {
    let mut endpoint = Endpoint::client("[::]:0".parse()?)?;
    endpoint.set_default_client_config(config);

    let connection = endpoint.connect(addr, "localhost")?.await?;

    Ok(connection)
}

pub async fn get_client_streams(connection: Connection) -> Result<Streams> {
    let (write, read) = connection.open_bi().await?;

    let write_stream = FramedWrite::new(write, MessageCodec::new());
    let read_stream = FramedRead::new(read, MessageCodec::new());

    Ok((write_stream, read_stream))
}
