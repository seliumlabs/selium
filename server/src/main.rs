use anyhow::{anyhow, bail, Result};
use clap::{Args, Parser};
use clap_verbosity_flag::Verbosity;
use dashmap::DashMap;
use env_logger::Builder;
use futures::{channel::mpsc, future, StreamExt, TryStreamExt};
use log::{error, info};
use ordered_sink::OrderedExt;
use pipeline::Pipeline;
use quinn::{IdleTimeout, VarInt};
use selium::{
    protocol::{Frame, PublisherPayload, SubscriberPayload},
    BiStream,
};
use std::{
    net::SocketAddr,
    path::PathBuf,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};

mod graph;
mod ordered_sink;
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
    let opts = quic::ConfigOptions {
        keylog: args.keylog,
        stateless_retry: args.stateless_retry,
        max_idle_timeout: IdleTimeout::from(VarInt::from_u32(args.max_idle_timeout)),
    };
    let config = quic::server_config(certs, key, opts)?;
    let endpoint = quinn::Endpoint::server(config, args.bind_addr)?;

    // Create hash to store message ordering data
    let topic_seq = Arc::new(DashMap::new());

    while let Some(conn) = endpoint.accept().await {
        info!("connection incoming");
        let pipe_clone = pipeline.clone();
        let topic_seq_clone = topic_seq.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_connection(pipe_clone.clone(), topic_seq_clone, conn).await {
                error!("connection failed: {reason}", reason = e.to_string());
            }
        });
    }

    Ok(())
}

async fn handle_connection(
    pipeline: Pipeline,
    topic_seq: Arc<DashMap<String, AtomicUsize>>,
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
        let stream = connection.accept_bi().await;
        let stream = match stream {
            Err(quinn::ConnectionError::ApplicationClosed { .. }) => {
                info!("Connection closed");
                return Ok(());
            }
            Err(e) => {
                bail!(e);
            }
            Ok(stream) => BiStream::from(stream),
        };

        let pipe_clone = pipeline.clone();
        let topic_seq_clone = topic_seq.clone();
        let addr = connection.remote_address();
        let stream_id = stream.read.get_ref().id();
        let stream_hash = format!("{addr}:{stream_id}");

        tokio::spawn(async move {
            if let Err(e) = handle_request(pipe_clone, topic_seq_clone, &stream_hash, stream).await
            {
                error!("Request failed: {reason}", reason = e.to_string());
            }
        });
    }
}

async fn handle_request(
    pipeline: Pipeline,
    topic_seq: Arc<DashMap<String, AtomicUsize>>,
    stream_hash: &str,
    mut stream: BiStream,
) -> Result<()> {
    // Receive header
    if let Some(result) = stream.next().await {
        match result? {
            Frame::RegisterPublisher(payload) => {
                handle_publisher(payload, pipeline, topic_seq, stream_hash, stream).await?
            }
            Frame::RegisterSubscriber(payload) => {
                handle_subscriber(payload, pipeline, stream_hash, stream).await?
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
    topic_seq: Arc<DashMap<String, AtomicUsize>>,
    stream_hash: &str,
    stream: BiStream,
) -> Result<()> {
    // Get current or create new sequence number for this topic
    let sequence = topic_seq
        .entry(header.topic.clone())
        .or_insert(AtomicUsize::new(1));

    pipeline.add_publisher(stream_hash, header)?;

    stream
        .try_for_each(move |frame| match frame {
            Frame::Message(bytes) => {
                let seq = sequence.fetch_add(1, Ordering::SeqCst);
                tokio::spawn(pipeline.traverse(stream_hash, bytes, seq));
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
    stream_hash: &str,
    stream: BiStream,
) -> Result<()> {
    let (tx_chan, rx_chan) = mpsc::unbounded();
    pipeline.add_subscriber(stream_hash, header, tx_chan)?;

    rx_chan
        .map(|(seq, bytes)| Ok((seq, Frame::Message(bytes))))
        .forward(stream.ordered())
        .await?;

    Ok(())
}
