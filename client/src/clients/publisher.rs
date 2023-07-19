use crate::aliases::Streams;
use crate::crypto::cert::load_root_store;
use crate::protocol::{Frame, PublisherPayload};
use crate::traits::{ClientAuth, Connect, IntoTimestamp, Operations};
use crate::utils::client::{configure_client, get_client_connection, get_client_streams};
use crate::utils::net::get_socket_addrs;
use crate::Operation;
use anyhow::Result;
use async_trait::async_trait;
use futures::{SinkExt, Sink};
use rustls::RootCertStore;
use std::path::PathBuf;
use std::pin::Pin;
use std::task::{Context, Poll};

#[derive(Debug)]
pub struct WantsCert {
    topic: String,
    retention_policy: u64,
    operations: Vec<Operation>,
}

#[derive(Debug)]
pub struct HasCert {
    topic: String,
    retention_policy: u64,
    operations: Vec<Operation>,
    root_store: RootCertStore,
}

pub fn publisher(topic: &str) -> PublisherBuilder<WantsCert> {
    PublisherBuilder {
        state: WantsCert {
            topic: topic.to_owned(),
            retention_policy: 0,
            operations: Vec::new(),
        },
    }
}

pub struct PublisherBuilder<T> {
    state: T,
}

impl PublisherBuilder<WantsCert> {
    pub fn retain<T: IntoTimestamp>(mut self, policy: T) -> Self {
        self.state.retention_policy = policy.into_timestamp();
        self
    }
}

impl Operations for PublisherBuilder<WantsCert> {
    fn map(mut self, module_path: &str) -> Self {
        self.state
            .operations
            .push(Operation::Map(module_path.into()));
        self
    }

    fn filter(mut self, module_path: &str) -> Self {
        self.state
            .operations
            .push(Operation::Filter(module_path.into()));
        self
    }
}

impl ClientAuth for PublisherBuilder<WantsCert> {
    type Output = PublisherBuilder<HasCert>;

    fn with_certificate_authority<T: Into<PathBuf>>(self, ca_path: T) -> Result<Self::Output> {
        let root_store = load_root_store(&ca_path.into())?;

        let state = HasCert {
            topic: self.state.topic,
            retention_policy: self.state.retention_policy,
            operations: self.state.operations,
            root_store,
        };

        Ok(PublisherBuilder { state })
    }
}

#[async_trait]
impl Connect for PublisherBuilder<HasCert> {
    type Output = Publisher;

    async fn connect(self, host: &str) -> Result<Self::Output> {
        let addr = get_socket_addrs(host)?;
        let config = configure_client(&self.state.root_store)?;
        let connection = get_client_connection(config, addr).await?;
        let mut streams = get_client_streams(connection).await?;

        register_publisher(self, &mut streams).await?;

        Ok(Publisher { streams })
    }
}

pub struct Publisher {
    streams: Streams,
}

impl Publisher {
    pub async fn finish(self) -> Result<()> {
        let (write, _) = self.streams;

        write.into_inner().finish().await?;

        Ok(())
    }
}

impl Sink<&str> for Publisher {
    type Error = anyhow::Error;

    fn poll_ready(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<()>> {
       self.streams.0.poll_ready_unpin(cx) 
    }

    fn start_send(mut self: Pin<&mut Self>, item: &str) -> Result<()> {
       self.streams.0.start_send_unpin(Frame::Message(item.to_owned())) 
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<()>> {
       self.streams.0.poll_flush_unpin(cx) 
    }

    fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<()>> {
       self.streams.0.poll_close_unpin(cx) 
    }
}

pub async fn register_publisher(
    client: PublisherBuilder<HasCert>,
    streams: &mut Streams,
) -> Result<()> {
    let (ref mut write, _) = streams;

    let frame = Frame::RegisterPublisher(PublisherPayload {
        topic: client.state.topic,
        retention_policy: client.state.retention_policy,
        operations: client.state.operations,
    });

    write.send(frame).await?;

    Ok(())
}
