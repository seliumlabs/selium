use crate::args::UserArgs;
use crate::quic::{load_root_store, read_certs, server_config, ConfigOptions};
use crate::topic::{pubsub, reqrep, Sender, Socket};
use anyhow::{anyhow, bail, Context, Result};
use futures::{future::join_all, stream::FuturesUnordered, SinkExt, StreamExt};
use log::{debug, error, info};
use quinn::{Connecting, Connection, Endpoint, IdleTimeout, VarInt};
use selium_protocol::{error_codes, BiStream, Frame, TopicName};
use selium_std::errors::SeliumError;
use std::{collections::HashMap, sync::Arc};
use tokio::{sync::Mutex, task::JoinHandle};

type TopicChannel = Sender<Frame, SeliumError>;
pub(crate) type SharedTopics = Arc<Mutex<HashMap<TopicName, TopicChannel>>>;
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
            if let Err(e) =
                handle_stream(topics_clone, topic_handles_clone, stream, connection).await
            {
                error!("Request failed: {:?}", e);
            }
        });
    }
}

async fn handle_stream(
    topics: SharedTopics,
    topic_handles: SharedTopicHandles,
    mut stream: BiStream,
    _connection: Connection,
) -> Result<()> {
    // Receive header
    if let Some(result) = stream.next().await {
        let frame = result?;
        let topic = frame.get_topic().ok_or(anyhow!("Expected header frame"))?;

        // Note this can only occur if someone circumvents the client lib
        if !topic.is_valid() {
            return Err(anyhow!("Invalid topic name"));
        }

        #[cfg(feature = "__cloud")]
        {
            use crate::cloud::do_cloud_auth;
            if let Err(e) = do_cloud_auth(&_connection, topic, &topics).await {
                debug!("Cloud authentication error: {e:?}");
                dbg!(&e);

                stream
                    .send(Frame::Error(e.to_string().into_bytes().into()))
                    .await?;

                return Ok(());
            }
        }

        let mut ts = topics.lock().await;

        // Spawn new topic if it doesn't exist yet
        if !ts.contains_key(&topic) {
            match frame {
                Frame::RegisterPublisher(_) | Frame::RegisterSubscriber(_) => {
                    let (fut, tx) = pubsub::Topic::pair();
                    let handle = tokio::spawn(fut);

                    topic_handles.lock().await.push(handle);
                    ts.insert(topic.clone(), Sender::Pubsub(tx));
                }
                Frame::RegisterReplier(_) | Frame::RegisterRequestor(_) => {
                    let (fut, tx) = reqrep::Topic::pair();
                    let handle = tokio::spawn(fut);

                    topic_handles.lock().await.push(handle);
                    ts.insert(topic.clone(), Sender::ReqRep(tx));
                }
                _ => unreachable!(), // because of `topic` instantiation
            };
        }

        let tx = ts.get_mut(&topic).unwrap();

        match frame {
            Frame::RegisterPublisher(_) => {
                let (_, read) = stream.split();
                tx.send(Socket::Pubsub(pubsub::Socket::Stream(Box::pin(read))))
                    .await
                    .context("Failed to add Publisher stream")?;
            }
            Frame::RegisterSubscriber(_) => {
                let (write, _) = stream.split();
                tx.send(Socket::Pubsub(pubsub::Socket::Sink(Box::pin(write))))
                    .await
                    .context("Failed to add Subscriber sink")?;
            }
            Frame::RegisterReplier(_) => {
                let (si, st) = stream.split();
                tx.send(Socket::Reqrep(reqrep::Socket::Server((
                    Box::pin(si),
                    Box::pin(st),
                ))))
                .await
                .context("Failed to add Replier")?;
            }
            Frame::RegisterRequestor(_) => {
                let (si, st) = stream.split();
                tx.send(Socket::Reqrep(reqrep::Socket::Client((
                    Box::pin(si),
                    Box::pin(st),
                ))))
                .await
                .context("Failed to add Requestor")?;
            }
            _ => unreachable!(), // because of `topic` instantiation
        }
    } else {
        info!("Stream closed");
    }

    Ok(())
}
