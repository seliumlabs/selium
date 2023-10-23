use crate::args::UserArgs;
use crate::quic::{load_root_store, read_certs, server_config, ConfigOptions};
use crate::topic::{Socket, Topic};
use anyhow::{anyhow, bail, Context, Result};
use futures::{channel::mpsc::Sender, SinkExt, StreamExt};
use log::{error, info};
use quinn::{Endpoint, IdleTimeout, VarInt};
use selium_protocol::{BiStream, Frame};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio_stream::StreamNotifyClose;

type TopicChannel = Sender<Socket<StreamNotifyClose<BiStream>, BiStream>>;
type SharedTopics = Arc<Mutex<HashMap<String, TopicChannel>>>;

pub struct Server {
    topics: SharedTopics,
    endpoint: Endpoint,
}

impl Server {
    pub async fn listen(&self) -> Result<()> {
        while let Some(conn) = self.endpoint.accept().await {
            info!("connection incoming");
            let topics_clone = self.topics.clone();
            tokio::spawn(async move {
                if let Err(e) = handle_connection(topics_clone, conn).await {
                    error!("connection failed: {:?}", e);
                }
            });
        }

        Ok(())
    }
}

impl TryFrom<UserArgs> for Server {
    type Error = anyhow::Error;

    fn try_from(args: UserArgs) -> Result<Self, Self::Error> {
        let root_store = load_root_store(args.cert.ca)?;
        let (certs, key) = read_certs(args.cert.cert, args.cert.key)?;

        let opts = ConfigOptions {
            keylog: args.keylog,
            stateless_retry: args.stateless_retry,
            max_idle_timeout: IdleTimeout::from(VarInt::from_u32(args.max_idle_timeout)),
        };

        let config = server_config(root_store, certs, key, opts)?;
        let endpoint = Endpoint::server(config, args.bind_addr)?;

        // Create hash to store message ordering data
        let topics = Arc::new(Mutex::new(HashMap::new()));

        Ok(Self { topics, endpoint })
    }
}

async fn handle_connection(topics: SharedTopics, conn: quinn::Connecting) -> Result<()> {
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

        tokio::spawn(async move {
            if let Err(e) = handle_stream(topics_clone, stream).await {
                error!("Request failed: {:?}", e);
            }
        });
    }
}

async fn handle_stream(
    topics: Arc<Mutex<HashMap<String, TopicChannel>>>,
    mut stream: BiStream,
) -> Result<()> {
    // Receive header
    if let Some(result) = stream.next().await {
        let frame = result?;
        let topic_name = frame.get_topic().ok_or(anyhow!("Expected header frame"))?;

        // Spawn new topic if it doesn't exist yet
        let mut ts = topics.lock().await;
        if !ts.contains_key(topic_name) {
            let (fut, tx) = Topic::pair();
            tokio::spawn(fut);

            ts.insert(topic_name.to_owned(), tx);
        }

        let tx = ts.get_mut(topic_name).unwrap();

        match frame {
            Frame::RegisterPublisher(_) => {
                tx.send(Socket::Stream(StreamNotifyClose::new(stream)))
                    .await
                    .context("Failed to add Publisher sink")?;
            }
            Frame::RegisterSubscriber(_) => {
                tx.send(Socket::Sink(stream))
                    .await
                    .context("Failed to add Subscriber sink")?;
            }
            _ => unreachable!(), // because of `topic_name` instantiation
        }
    } else {
        info!("Stream closed");
    }

    Ok(())
}
