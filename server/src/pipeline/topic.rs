use anyhow::Result;
use quinn::StreamId;
use selium_common::protocol::{SubscriberPayload, PublisherPayload};
use selium_common::types::BiStream;
use tokio::sync::RwLock;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::ops::Deref;
use std::sync::Arc;
use super::channels::Channels;
use super::publisher::Publisher;
use super::subscriber::Subscriber;

#[derive(Debug, Eq, PartialEq, Hash)]
pub struct SockHash(String);

impl Deref for SockHash {
    type Target = String;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

pub type PeerMap<T> = Arc<RwLock<HashMap<SockHash, T>>>;

pub struct Topic {
    publishers: PeerMap<Publisher>,
    subscribers: PeerMap<Subscriber>,
    channels: Channels,
}

impl Default for Topic {
    fn default() -> Self {
        Topic {
            publishers: Arc::new(RwLock::new(HashMap::new())),
            subscribers: Arc::new(RwLock::new(HashMap::new())),
            channels: Channels::new(),
        }
    }
}

impl Topic {
    pub async fn add_publisher(&mut self, header: PublisherPayload, conn_addr: SocketAddr, stream: BiStream) -> Result<()> {
        let mut publishers = self.publishers.write().await;
        let hash = sock_key(conn_addr, stream.get_recv_stream_id());
        let publisher = Publisher::new(stream, header.operations);

        publishers.insert(hash, publisher.clone());
        drop(publishers);

        self.channels.add_stream(publisher).await?;

        if self.has_peers().await {
            self.channels.spawn();
        }

        Ok(())
    }

    pub async fn add_subscriber(&mut self, header: SubscriberPayload, conn_addr: SocketAddr, sink: BiStream) -> Result<()> {
        let mut subscribers = self.subscribers.write().await;
        let hash = sock_key(conn_addr, sink.get_send_stream_id());
        let subscriber = Subscriber::new(sink, header.operations);

        subscribers.insert(hash, subscriber.clone());
        drop(subscribers);
        
        self.channels.add_sink(subscriber).await?;

        if self.has_peers().await {
            self.channels.spawn();
        }

        Ok(())
    }

    pub async fn rm_publisher(&self, sock_hash: &SockHash) -> Result<()> {
        self.publishers.write().await.remove(sock_hash);

        Ok(())
    }

    pub async fn rm_subscriber(&self, sock_hash: &SockHash) -> Result<()> {
        self.subscribers.write().await.remove(sock_hash);

        Ok(())
    }

    async fn has_peers(&self) -> bool {
        let publishers = self.publishers.read().await;
        let subscribers = self.subscribers.read().await;

        !publishers.is_empty() && !subscribers.is_empty()
    }
}

fn sock_key(conn_addr: SocketAddr, stream_id: StreamId) -> SockHash {
    SockHash(format!("{conn_addr}:{stream_id}"))
}

