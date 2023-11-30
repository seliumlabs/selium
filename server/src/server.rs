use std::{collections::HashMap, sync::Arc};

use crate::args::UserArgs;
use crate::quic::{load_root_store, read_certs, server_config, ConfigOptions};
use crate::topic::{pubsub, reqrep, Sender, Socket};
use anyhow::{anyhow, bail, Context, Result};
use futures::{future::join_all, stream::FuturesUnordered, StreamExt};
use log::{error, info};
use quinn::{Connecting, Endpoint, IdleTimeout, VarInt};
use selium_protocol::{error_codes, BiStream, Frame, ReadHalf, WriteHalf};
use tokio::{sync::Mutex, task::JoinHandle};

type TopicChannel = Sender<ReadHalf, WriteHalf>;
type SharedTopics = Arc<Mutex<HashMap<String, TopicChannel>>>;
type SharedTopicHandles = Arc<Mutex<FuturesUnordered<JoinHandle<()>>>>;

pub struct Server {
    topics: SharedTopics,
    topic_handles: SharedTopicHandles,
    endpoint: Endpoint,
}

impl Server {
    pub async fn listen(&self) -> Result<()> {
        loop {
            tokio::select! {
                Some(conn) = self.endpoint.accept() => {
                    self.connect(conn).await?;
                },
                Ok(()) = tokio::signal::ctrl_c() => {
                    self.shutdown().await?;
                    break;
                }
            }
        }

        Ok(())
    }

    async fn connect(&self, conn: Connecting) -> Result<()> {
        info!("connection incoming");
        let topics_clone = self.topics.clone();
        let topic_handles = self.topic_handles.clone();

        tokio::spawn(async move {
            if let Err(e) = handle_connection(topics_clone, topic_handles, conn).await {
                error!("connection failed: {:?}", e);
            }
        });

        Ok(())
    }

    async fn shutdown(&self) -> Result<()> {
        info!("Shutdown signal received: preparing to gracefully shutdown.");
        self.endpoint.reject_new_connections();

        let mut topics = self.topics.lock().await;
        let mut topic_handles = self.topic_handles.lock().await;

        topics.values_mut().for_each(|t| t.close_channel());
        join_all(topic_handles.iter_mut()).await;

        self.endpoint
            .close(error_codes::SHUTDOWN, b"Scheduled shutdown.");
        self.endpoint.wait_idle().await;

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
        let topic_handles = Arc::new(Mutex::new(FuturesUnordered::new()));

        Ok(Self {
            topics,
            topic_handles,
            endpoint,
        })
    }
}

async fn handle_connection(
    topics: SharedTopics,
    topic_handles: SharedTopicHandles,
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
        let topic_handles_clone = topic_handles.clone();

        tokio::spawn(async move {
            if let Err(e) = handle_stream(topics_clone, topic_handles_clone, stream).await {
                error!("Request failed: {:?}", e);
            }
        });
    }
}

async fn handle_stream(
    topics: Arc<Mutex<HashMap<String, TopicChannel>>>,
    topic_handles: SharedTopicHandles,
    mut stream: BiStream,
) -> Result<()> {
    // Receive header
    if let Some(result) = stream.next().await {
        let frame = result?;
        let topic_name = frame.get_topic().ok_or(anyhow!("Expected header frame"))?;

        // Spawn new topic if it doesn't exist yet
        let mut ts = topics.lock().await;
        if !ts.contains_key(topic_name) {
            match frame {
                Frame::RegisterPublisher(_) | Frame::RegisterSubscriber(_) => {
                    let (fut, tx) = pubsub::Topic::pair();
                    let topic = tokio::spawn(fut);

                    topic_handles.lock().await.push(topic);
                    ts.insert(topic_name.to_owned(), Sender::Pubsub(tx));
                }
                Frame::RegisterReplier(_) | Frame::RegisterRequestor(_) => {
                    let (fut, tx) = reqrep::Topic::pair();
                    let topic = tokio::spawn(fut);

                    topic_handles.lock().await.push(topic);
                    ts.insert(topic_name.to_owned(), Sender::ReqRep(tx));
                }
                _ => unreachable!(), // because of `topic_name` instantiation
            };
        }

        let tx = ts.get_mut(topic_name).unwrap();

        match frame {
            Frame::RegisterPublisher(_) => {
                let (_, read) = stream.split();
                tx.send(Socket::Pubsub(pubsub::Socket::Stream(read)))
                    .await
                    .context("Failed to add Publisher stream")?;
            }
            Frame::RegisterSubscriber(_) => {
                let (write, _) = stream.split();
                tx.send(Socket::Pubsub(pubsub::Socket::Sink(write)))
                    .await
                    .context("Failed to add Subscriber sink")?;
            }
            Frame::RegisterReplier(_) => {
                tx.send(Socket::Reqrep(reqrep::Socket::Server(stream)))
                    .await
                    .context("Failed to add Replier")?;
            }
            Frame::RegisterRequestor(_) => {
                tx.send(Socket::Reqrep(reqrep::Socket::Client(stream)))
                    .await
                    .context("Failed to add Requestor")?;
            }
            _ => unreachable!(), // because of `topic_name` instantiation
        }
    } else {
        info!("Stream closed");
    }

    Ok(())
}
