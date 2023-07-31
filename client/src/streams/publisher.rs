use super::builder::{StreamBuilder, StreamCommon};
use crate::traits::{MessageEncoder, Open, StreamConfig, TryIntoU64};
use anyhow::Result;
use async_trait::async_trait;
use futures::{Sink, SinkExt};
use quinn::Connection;
use selium_common::protocol::{Frame, PublisherPayload};
use selium_common::types::BiStream;
use std::marker::PhantomData;
use std::pin::Pin;
use std::task::{Context, Poll};

#[doc(hidden)]
#[derive(Debug)]
pub struct PublisherWantsEncoder {
    pub(crate) common: StreamCommon,
}

#[doc(hidden)]
#[derive(Debug)]
pub struct PublisherWantsOpen<E, Item> {
    common: StreamCommon,
    encoder: E,
    _marker: PhantomData<Item>,
}

impl StreamConfig for StreamBuilder<PublisherWantsEncoder> {
    fn map(mut self, module_path: &str) -> Self {
        self.state.common.map(module_path);
        self
    }

    fn filter(mut self, module_path: &str) -> Self {
        self.state.common.filter(module_path);
        self
    }

    fn retain<T: TryIntoU64>(mut self, policy: T) -> Result<Self> {
        self.state.common.retain(policy)?;
        Ok(self)
    }
}

impl StreamBuilder<PublisherWantsEncoder> {
    pub fn with_encoder<E, Item>(self, encoder: E) -> StreamBuilder<PublisherWantsOpen<E, Item>> {
        let state = PublisherWantsOpen {
            common: self.state.common,
            encoder,
            _marker: PhantomData,
        };

        StreamBuilder {
            state,
            connection: self.connection,
        }
    }
}

#[async_trait]
impl<E, Item> Open for StreamBuilder<PublisherWantsOpen<E, Item>>
where
    E: MessageEncoder<Item> + Send + Clone,
    Item: Send,
{
    type Output = Publisher<E, Item>;

    async fn open(self) -> Result<Self::Output> {
        let headers = PublisherPayload {
            topic: self.state.common.topic,
            retention_policy: self.state.common.retention_policy,
            operations: self.state.common.operations,
        };

        let publisher = Publisher::spawn(self.connection, headers, self.state.encoder).await?;

        Ok(publisher)
    }
}

pub struct Publisher<E, Item> {
    connection: Connection,
    stream: BiStream,
    headers: PublisherPayload,
    encoder: E,
    _marker: PhantomData<Item>,
}

impl<E, Item> Publisher<E, Item>
where
    E: MessageEncoder<Item> + Clone,
{
    pub async fn spawn(
        connection: Connection,
        headers: PublisherPayload,
        encoder: E,
    ) -> Result<Self> {
        let mut stream = BiStream::try_from_connection(&connection).await?;
        let frame = Frame::RegisterPublisher(headers.clone());
        stream.send(frame).await?;

        Ok(Self {
            connection,
            stream,
            headers,
            encoder,
            _marker: PhantomData,
        })
    }

    pub async fn duplicate(&self) -> Result<Self> {
        let publisher = Publisher::spawn(
            self.connection.clone(),
            self.headers.clone(),
            self.encoder.clone(),
        )
        .await?;

        Ok(publisher)
    }

    pub async fn finish(mut self) -> Result<()> {
        self.stream.finish().await
    }
}

impl<E, Item> Sink<Item> for Publisher<E, Item>
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
