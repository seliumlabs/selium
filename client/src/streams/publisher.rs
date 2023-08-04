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
    /// Specifies the encoder a [Publisher](crate::Publisher) uses for encoding produced messages prior
    /// to being sent over the wire.
    ///
    /// An encoder can be any type implementing
    /// [MessageEncoder](crate::traits::MessageEncoder). See [codecs](crate::codecs) for a list of
    /// codecs available in `Selium`, along with tutorials for creating your own encoders.
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

/// A traditional publisher stream that produces and sends messages to a topic.
///
/// A Publisher is different to a [Subscriber](crate::Subscriber), in that it holds a reference
/// to the client connection handle in order to facilitate duplicating the stream. This makes it
/// possible to spawn branching streams to concurrently publish messages to the same topic.
///
/// The Publisher struct implements the [futures::Sink] trait, and can thus be used in the same
/// contexts as a [Sink](futures::Sink). Any messages sent to the sink will be encoded with the
/// provided encoder, before being sent over the wire.
///
/// **Note:** The Publisher struct is never constructed directly, but rather, via a
/// [StreamBuilder](crate::StreamBuilder).
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
    async fn spawn(connection: Connection, headers: PublisherPayload, encoder: E) -> Result<Self> {
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

    /// Spawns a new [Publisher] stream with the same configuration as the current stream, without
    /// having to specify the same configuration options again.
    ///  
    /// This method is especially convenient in cases where multiple streams will be concurrently
    /// publishing messages to the same topic, via cooperative-multitasking, e.g. within [tokio] tasks
    /// spawned by [tokio::spawn].
    ///
    /// See the included examples in the repository for more information.
    ///
    /// # Errors
    ///
    /// Returns [Err] if a new stream cannot be opened on the current client connection.
    pub async fn duplicate(&self) -> Result<Self> {
        let publisher = Publisher::spawn(
            self.connection.clone(),
            self.headers.clone(),
            self.encoder.clone(),
        )
        .await?;

        Ok(publisher)
    }

    /// Gracefully closes the stream.
    ///
    /// It is highly recommended to invoke this method after no new messages will be
    /// published to this stream. This is to assure that the `Selium` server has acknowledged all
    /// sent data prior to closing the connection.
    ///
    /// Under the hood, `finish` calls the [finish](quinn::SendStream::finish) on the underlying
    /// [SendStream](quinn::SendStream).
    ///
    /// # Errors
    ///
    /// Returns [Err] if the stream fails to close gracefully.
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
