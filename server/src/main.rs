use std::{net::SocketAddr, path::PathBuf};

use anyhow::{anyhow, bail, Result};
use clap::{Args, Parser};
use clap_verbosity_flag::Verbosity;
use env_logger::Builder;
use futures::{channel::mpsc, future, StreamExt, TryStreamExt};
use log::{error, info};
use pipeline::Pipeline;
use quinn::{IdleTimeout, RecvStream, SendStream, VarInt};
use selium::protocol::{Frame, MessageCodec, PublisherPayload, SubscriberPayload};
use tokio_util::codec::{FramedRead, FramedWrite};

mod graph;
mod pipeline;
mod quic;

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
    /// Maximum time a client can idle waiting for data - defaults to infinity
    #[clap(long = "max-idle-timeout", default_value_t = 15, value_parser = clap::value_parser!(u32).range(5..30))]
    max_idle_timeout: u32,
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
    logger.filter_level(args.verbose.log_level_filter()).init();

    let pipeline = Pipeline::new();

    let (certs, key) = if let (Some(cert_path), Some(key_path)) = (args.cert.cert, args.cert.key) {
        quic::read_certs(cert_path, key_path)?
    } else if args.cert.self_signed {
        quic::generate_self_signed_cert()?
    } else {
        // Clap ensures that either --cert + --key or --self-signed are present
        unreachable!();
    };
    let mut opts = quic::ConfigOptions::default();
    opts.keylog = args.keylog;
    opts.stateless_retry = args.stateless_retry;
    opts.max_idle_timeout = IdleTimeout::from(VarInt::from_u32(args.max_idle_timeout));
    let config = quic::server_config(certs, key, opts)?;
    let endpoint = quinn::Endpoint::server(config, args.bind_addr)?;

    while let Some(conn) = endpoint.accept().await {
        info!("connection incoming");
        let pipe_clone = pipeline.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_connection(pipe_clone.clone(), conn).await {
                error!("connection failed: {reason}", reason = e.to_string());
            }
        });
    }

    Ok(())
}

async fn handle_connection(pipeline: Pipeline, conn: quinn::Connecting) -> Result<()> {
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
        let stream = connection.accept_bi().await;
        let (tx, rx) = match stream {
            Err(quinn::ConnectionError::ApplicationClosed { .. }) => {
                info!("Connection closed");
                return Ok(());
            }
            Err(e) => {
                bail!(e);
            }
            Ok((tx, rx)) => (
                FramedWrite::new(tx, MessageCodec::new()),
                FramedRead::new(rx, MessageCodec::new()),
            ),
        };

        let pipe_clone = pipeline.clone();
        let addr = connection.remote_address();
        tokio::spawn(async move {
            if let Err(e) = handle_request(pipe_clone, addr, tx, rx).await {
                error!("Request failed: {reason}", reason = e.to_string());
            }
        });
    }
}

async fn handle_request(
    pipeline: Pipeline,
    addr: SocketAddr,
    tx: FramedWrite<SendStream, MessageCodec>,
    mut rx: FramedRead<RecvStream, MessageCodec>,
) -> Result<()> {
    // Receive header
    if let Some(result) = rx.next().await {
        match result? {
            Frame::RegisterPublisher(payload) => {
                handle_publisher(payload, pipeline.clone(), addr, rx).await?
            }
            Frame::RegisterSubscriber(payload) => {
                handle_subscriber(payload, pipeline, addr, tx).await?
            }
            _ => return Err(anyhow!("Non header frame received out of context")),
        }
    } else {
        info!("Socket closed");
    }

    Ok(())
}

async fn handle_publisher(
    header: PublisherPayload,
    pipeline: Pipeline,
    addr: SocketAddr,
    rx: FramedRead<RecvStream, MessageCodec>,
) -> Result<()> {
    pipeline.add_publisher(addr, header);

    rx.try_for_each(move |frame| match frame {
        Frame::Message(msg) => {
            tokio::spawn(pipeline.traverse(addr, msg));
            future::ok(())
        }
        _ => future::err(anyhow!("Non Message frame received out of context")),
    })
    .await?;
    Ok(())
}

async fn handle_subscriber(
    header: SubscriberPayload,
    pipeline: Pipeline,
    addr: SocketAddr,
    tx: FramedWrite<SendStream, MessageCodec>,
) -> Result<()> {
    let (tx_chan, rx_chan) = mpsc::unbounded();
    pipeline.add_subscriber(addr, header, tx_chan);

    rx_chan
        .map(|msg| Ok(Frame::Message(msg)))
        .forward(tx)
        .await?;

    Ok(())
}
