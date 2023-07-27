use super::builder::{ClientBuilder, ClientCommon};
use crate::crypto::cert::load_root_store;
use crate::protocol::{Frame, PublisherPayload};
use crate::traits::{Client, ClientConfig, Connect, MessageEncoder, TryIntoU64};
use crate::utils::client::establish_connection;
use crate::BiStream;
use anyhow::Result;
use async_trait::async_trait;
use futures::{Sink, SinkExt};
use quinn::Connection;
use rustls::RootCertStore;
use std::marker::PhantomData;
use std::path::PathBuf;
use std::pin::Pin;
use std::task::{Context, Poll};

pub struct PublisherWantsCert {
    common: ClientCommon,
}

pub struct PublisherWantsEncoder {
    common: ClientCommon,
    root_store: RootCertStore,
}

pub struct PublisherReady<E, Item> {
    common: ClientCommon,
    encoder: E,
    root_store: RootCertStore,
    _marker: PhantomData<Item>,
}

pub fn publisher(topic: &str) -> ClientBuilder<PublisherWantsCert> {
    ClientBuilder {
        state: PublisherWantsCert {
            common: ClientCommon::new(topic),
        },
    }
}

impl ClientConfig for ClientBuilder<PublisherWantsCert> {
    type NextState = ClientBuilder<PublisherWantsEncoder>;

    fn map(mut self, module_path: &str) -> Self {
        self.state.common.map(module_path);
        self
    }

    fn filter(mut self, module_path: &str) -> Self {
        self.state.common.filter(module_path);
        self
    }

    fn keep_alive<T: TryIntoU64>(mut self, interval: T) -> Result<Self> {
        self.state.common.keep_alive(interval)?;
        Ok(self)
    }

    fn retain<T: TryIntoU64>(mut self, policy: T) -> Result<Self> {
        self.state.common.retain(policy)?;
        Ok(self)
    }

    fn with_certificate_authority<T: Into<PathBuf>>(self, ca_path: T) -> Result<Self::NextState> {
        let root_store = load_root_store(&ca_path.into())?;

        let state = PublisherWantsEncoder {
            common: self.state.common,
            root_store,
        };

        Ok(ClientBuilder { state })
    }
}

impl ClientBuilder<PublisherWantsEncoder> {
    pub fn with_encoder<E, Item>(self, encoder: E) -> ClientBuilder<PublisherReady<E, Item>> {
        let state = PublisherReady {
            common: self.state.common,
            root_store: self.state.root_store,
            encoder,
            _marker: PhantomData,
        };

        ClientBuilder { state }
    }
}

#[async_trait]
impl<E, Item> Connect for ClientBuilder<PublisherReady<E, Item>>
where
    E: MessageEncoder<Item> + Send,
    Item: Send,
{
    type Output = Publisher<E, Item>;

    async fn connect(self, host: &str) -> Result<Self::Output> {
        let connection =
            establish_connection(host, &self.state.root_store, self.state.common.keep_alive)
                .await?;

        let headers = PublisherPayload {
            topic: self.state.common.topic,
            retention_policy: self.state.common.retention_policy,
            operations: self.state.common.operations,
        };

        Ok(Publisher {
            connection,
            headers,
            encoder: self.state.encoder,
            _marker: PhantomData,
        })
    }
}

pub struct Publisher<E, Item> {
    connection: Connection,
    headers: PublisherPayload,
    encoder: E,
    _marker: PhantomData<Item>,
}

impl<E, Item> Publisher<E, Item>
where
    E: MessageEncoder<Item> + Clone,
{
    pub async fn stream(&self) -> Result<PublisherStream<E, Item>> {
        let mut stream = BiStream::try_from_connection(self.connection.clone()).await?;
        let frame = Frame::RegisterPublisher(self.headers.clone());

        stream.send(frame).await?;

        Ok(PublisherStream {
            stream,
            encoder: self.encoder.clone(),
            _marker: PhantomData,
        })
    }
}

pub struct PublisherStream<E, Item> {
    stream: BiStream,
    encoder: E,
    _marker: PhantomData<Item>,
}

#[async_trait]
impl<E, Item> Client for PublisherStream<E, Item>
where
    E: MessageEncoder<Item> + Send,
    Item: Send,
{
    async fn finish(self) -> Result<()> {
        self.stream.finish().await
    }
}

impl<E, Item> Sink<Item> for PublisherStream<E, Item>
where
    E: MessageEncoder<Item> + Send + Unpin,
    Item: Unpin,
{
    type Error = anyhow::Error;

    fn poll_ready(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<()>> {
        self.stream.poll_ready_unpin(cx)
    }

    fn start_send(mut self: Pin<&mut Self>, item: Item) -> Result<()> {
        let bytes = self.encoder.encode(item)?;
        self.stream.start_send_unpin(Frame::Message(bytes))
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<()>> {
        self.stream.poll_flush_unpin(cx)
    }

    fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<()>> {
        self.stream.poll_close_unpin(cx)
    }
}
