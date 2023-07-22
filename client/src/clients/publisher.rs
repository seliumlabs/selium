use crate::aliases::Streams;
use crate::crypto::cert::load_root_store;
use crate::protocol::{Frame, PublisherPayload};
use crate::traits::{Connect, IntoTimestamp, ClientConfig, Client};
use crate::utils::client::{configure_client, get_client_connection, get_client_streams};
use crate::utils::net::get_socket_addrs;
use anyhow::Result;
use async_trait::async_trait;
use futures::{SinkExt, Sink};
use rustls::RootCertStore;
use std::path::PathBuf;
use std::pin::Pin;
use std::task::{Context, Poll};
use super::builder::{ClientBuilder, ClientCommon};

pub const RETENTION_POLICY_DEFAULT: u64 = 0;

#[derive(Debug)]
pub struct PublisherWantsCert {
    common: ClientCommon,
    retention_policy: u64,
}

#[derive(Debug)]
pub struct PublisherHasCert {
    common: ClientCommon,
    retention_policy: u64,
    root_store: RootCertStore,
}

pub fn publisher(topic: &str) -> ClientBuilder<PublisherWantsCert> {
    ClientBuilder {
        state: PublisherWantsCert {
            common: ClientCommon::new(topic),
            retention_policy: RETENTION_POLICY_DEFAULT,
        },
    }
}

impl ClientBuilder<PublisherWantsCert> {
    pub fn retain<T: IntoTimestamp>(mut self, policy: T) -> Self {
        self.state.retention_policy = policy.into_timestamp();
        self
    }
}

impl ClientConfig for ClientBuilder<PublisherWantsCert> {
    type NextState = ClientBuilder<PublisherHasCert>;

    fn map(mut self, module_path: &str) -> Self {
        self.state.common.map(module_path);
        self
    }

    fn filter(mut self, module_path: &str) -> Self {
        self.state.common.filter(module_path);
        self
    }

    fn keep_alive<T: IntoTimestamp>(mut self, interval: T) -> Self {
        self.state.common.keep_alive(interval);
        self
    }

    fn with_certificate_authority<T: Into<PathBuf>>(self, ca_path: T) -> Result<Self::NextState> {
        let root_store = load_root_store(&ca_path.into())?;

        let state = PublisherHasCert {
            common: ClientCommon {
                topic: self.state.common.topic,
                keep_alive: self.state.common.keep_alive,
                operations: self.state.common.operations,
            },
            retention_policy: self.state.retention_policy,
            root_store,
        };

        Ok(ClientBuilder { state })
    }
}

#[async_trait]
impl Connect for ClientBuilder<PublisherHasCert> {
    type Output = Publisher;

    async fn connect(self, host: &str) -> Result<Self::Output> {
        let addr = get_socket_addrs(host)?;
        let config = configure_client(&self.state.root_store, self.state.common.keep_alive)?;
        let connection = get_client_connection(config, addr).await?;
        let mut streams = get_client_streams(connection).await?;

        register_publisher(self, &mut streams).await?;

        Ok(Publisher { streams })
    }
}

pub struct Publisher {
    streams: Streams,
}

#[async_trait]
impl Client for Publisher {
    async fn finish(self) -> Result<()> {
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
    client: ClientBuilder<PublisherHasCert>,
    streams: &mut Streams,
) -> Result<()> {
    let (ref mut write, _) = streams;

    let frame = Frame::RegisterPublisher(PublisherPayload {
        topic: client.state.common.topic,
        retention_policy: client.state.retention_policy,
        operations: client.state.common.operations,
    });

    write.send(frame).await?;

    Ok(())
}
