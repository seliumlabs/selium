use anyhow::{anyhow, bail, Context, Result};
use clap::{Args, Parser};
use clap_verbosity_flag::Verbosity;
use dashmap::DashMap;
use env_logger::Builder;
use futures::StreamExt;
use log::{error, info};
use pipeline::Pipeline;
use quinn::{Connection, IdleTimeout, VarInt};
use selium_common::{protocol::Frame, types::BiStream};
use std::{net::SocketAddr, path::PathBuf, sync::Arc};
use topic::Topic;

mod node;
mod quic;
mod sink;
mod stream;
mod topic;

#[derive(Parser, Debug)]
#[command(version, about)]
struct UserArgs {
    /// Address to bind this server to
    #[clap(short = 'a', long = "bind-addr")]
    bind_addr: SocketAddr,
    #[clap(flatten)]
    cert: CertGroup,
    /// Enable stateless retries
    #[clap(long = "stateless-retry")]
    stateless_retry: bool,
    /// File to log TLS keys to for debugging
    #[clap(long = "keylog")]
    keylog: bool,
    /// Maximum time in ms a client can idle waiting for data - default to 15 seconds
    #[clap(long = "max-idle-timeout", default_value_t = 15000, value_parser = clap::value_parser!(u32))]
    max_idle_timeout: u32,
    /// Can be called multiple times to increase output
    #[clap(flatten)]
    verbose: Verbosity,
}

#[derive(Args, Debug)]
#[group(required = true)]
struct CertGroup {
    /// TLS private key
    #[clap(short = 'k', long = "key", requires = "cert")]
    key: Option<PathBuf>,
    /// TLS certificate
    #[clap(short = 'c', long = "cert", requires = "key")]
    cert: Option<PathBuf>,
    /// Autogenerate server cert (NOTE: This should only be used for testing!)
    #[clap(long = "self-signed", conflicts_with = "cert")]
    self_signed: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = UserArgs::parse();

    let mut logger = Builder::new();
    logger
        .filter_module(
            &env!("CARGO_PKG_NAME").replace('-', "_"),
            args.verbose.log_level_filter(),
        )
        .init();

    let (certs, key) = if let (Some(cert_path), Some(key_path)) = (args.cert.cert, args.cert.key) {
        quic::read_certs(cert_path, key_path)?
    } else if args.cert.self_signed {
        quic::generate_self_signed_cert()?
    } else {
        // Clap ensures that either --cert + --key or --self-signed are present
        unreachable!();
    };
    let opts = quic::ConfigOptions {
        keylog: args.keylog,
        stateless_retry: args.stateless_retry,
        max_idle_timeout: IdleTimeout::from(VarInt::from_u32(args.max_idle_timeout)),
    };
    let config = quic::server_config(certs, key, opts)?;
    let endpoint = quinn::Endpoint::server(config, args.bind_addr)?;

    // Create hash to store message ordering data
    let topics = Arc::new(DashMap::new());

    while let Some(conn) = endpoint.accept().await {
        info!("connection incoming");
        let topics_clone = topics.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_connection(topics_clone, conn).await {
                error!("connection failed: {:?}", e);
            }
        });
    }

    Ok(())
}

async fn handle_connection(
    topics: Arc<DashMap<String, Topic>>,
    conn: quinn::Connecting,
) -> Result<()> {
    let connection = conn.await?;
    info!(
        "Connection {} - {}",
        connection.remote_address(),
        connection
            .handshake_data()
            .unwrap()
            .downcast::<quinn::crypto::rustls::HandshakeData>()
            .unwrap()
            .protocol
            .map_or_else(
                || "<none>".into(),
                |x| String::from_utf8_lossy(&x).into_owned()
            )
    );

    loop {
        let connection = connection.clone();
        let stream = connection.accept_bi().await;
        let stream = match stream {
            Err(quinn::ConnectionError::ApplicationClosed { .. }) => {
                info!("Connection closed ({})", connection.remote_address());
                return Ok(());
            }
            Err(e) => {
                bail!(e)
            }
            Ok(stream) => BiStream::from(stream),
        };

        let topics_clone = topics.clone();
        let addr = connection.remote_address();

        tokio::spawn(async move {
            if let Err(e) = handle_stream(addr, topics_clone, stream).await {
                error!("Request failed: {:?}", e);
            }
        });
    }
}

async fn handle_stream(
    conn_addr: SocketAddr,
    topics: Arc<DashMap<String, Topic>>,
    mut stream: BiStream,
) -> Result<()> {
    // Receive header
    if let Some(result) = stream.next().await {
        match result? {
            Frame::RegisterPublisher(payload) => {
                if !topics.contains_key(&payload.topic) {
                    topics.insert(payload.topic.clone(), Topic::default());
                }

                let mut topic = topics.get_mut(&payload.topic).unwrap();

                topic
                    .add_publisher(payload, conn_addr, stream)
                    .await
                    .context("Publisher error")?;
            }
            Frame::RegisterSubscriber(payload) => {
                if !topics.contains_key(&payload.topic) {
                    topics.insert(payload.topic.clone(), Topic::default());
                }

                let mut topic = topics.get_mut(&payload.topic).unwrap();

                topic
                    .add_subscriber(payload, conn_addr, stream)
                    .await
                    .context("Subscriber error")?;
            }
            _ => return Err(anyhow!("Expected Header frame")),
        }
    } else {
        info!("Stream closed");
    }

    Ok(())
}
