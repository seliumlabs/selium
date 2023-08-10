use super::channels::Channels;
use super::exclusive::Exclusive;
use anyhow::Result;
use quinn::StreamId;
use selium_common::protocol::{PublisherPayload, SubscriberPayload};
use selium_common::types::BiStream;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::ops::Deref;
use std::sync::Arc;
use tokio::sync::{RwLock, Mutex};

#[derive(Debug, Eq, PartialEq, Hash)]
pub struct SockHash(String);

impl Deref for SockHash {
    type Target = String;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

pub type PeerMap = Arc<RwLock<HashMap<SockHash, Arc<Mutex<BiStream>>>>>;

pub struct Topic {
    publishers: PeerMap,
    subscribers: PeerMap,
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
    pub async fn add_publisher(
        &mut self,
        header: PublisherPayload,
        conn_addr: SocketAddr,
        stream: BiStream,
    ) -> Result<()> {
        {
            let mut publishers = self.publishers.write().await;
            let hash = sock_key(conn_addr, stream.get_recv_stream_id());

            // @TODO - Support Operations struct
            let stream = Arc::new(Mutex::new(stream));
            publishers.insert(hash, stream.clone());

            self.channels.add_stream(Exclusive::new(stream)).await?;
        }

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
        {
            let mut subscribers = self.subscribers.write().await;
            let hash = sock_key(conn_addr, sink.get_send_stream_id());

            // @TODO - Support Operations struct
            let sink = Arc::new(Mutex::new(sink));
            subscribers.insert(hash, sink.clone());

            self.channels.add_sink(Exclusive::new(sink)).await?;
        }

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
