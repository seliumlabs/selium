use super::channels::Channels;
use super::lockable::Lockable;
use anyhow::Result;
use futures::{Sink, Stream};
use quinn::StreamId;
use selium_common::protocol::{PublisherPayload, SubscriberPayload};
use selium_common::types::BiStream;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::ops::Deref;
use std::sync::{Arc, Mutex};
use tokio::sync::RwLock;

#[derive(Debug, Eq, PartialEq, Hash)]
pub struct SockHash(String);

impl Deref for SockHash {
    type Target = String;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

pub type StreamPeerMap<T> = Arc<RwLock<HashMap<SockHash, Box<dyn Stream<Item = T>>>>>;
pub type SinkPeerMap<T, E> = Arc<RwLock<HashMap<SockHash, Box<dyn Sink<T, Error = E>>>>>;

pub struct Topic<Item> {
    publishers: StreamPeerMap,
    subscribers: SinkPeerMap<Item>,
    channels: Channels,
}

impl<Item> Default for Topic<Item> {
    fn default() -> Self {
        Topic {
            publishers: Arc::new(RwLock::new(HashMap::new())),
            subscribers: Arc::new(RwLock::new(HashMap::new())),
            channels: Channels::new(),
        }
    }
}

impl<Item> Topic<Item> {
    pub async fn add_publisher(
        &mut self,
        header: PublisherPayload,
        conn_addr: SocketAddr,
        stream: BiStream,
    ) -> Result<()> {
        let mut publishers = self.publishers.write().await;
        let hash = sock_key(conn_addr, stream.get_recv_stream_id());

        // @TODO - Support Operations struct
        let stream = Arc::new(Mutex::new(Box::new(stream)));
        publishers.insert(hash, stream.clone());

        self.channels.add_stream(Lockable::new(stream)).await?;

        if self.has_peers().await {
            self.channels.spawn();
        }

        Ok(())
    }

    pub async fn add_subscriber(
        &mut self,
        header: SubscriberPayload,
        conn_addr: SocketAddr,
        sink: BiStream,
    ) -> Result<()> {
        let mut subscribers = self.subscribers.write().await;
        let hash = sock_key(conn_addr, sink.get_send_stream_id());

        // @TODO - Support Operations struct
        let sink = Arc::new(Mutex::new(sink));
        subscribers.insert(hash, sink.clone());

        self.channels.add_sink(Lockable::new(sink)).await?;

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
