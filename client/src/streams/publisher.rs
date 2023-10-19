use super::builder::{StreamBuilder, StreamCommon};
use crate::batching::{BatchConfig, MessageBatch};
use crate::traits::{Open, Operations, Retain, TryIntoU64};
use anyhow::Result;
use async_trait::async_trait;
use bytes::Bytes;
use futures::{Sink, SinkExt};
use quinn::Connection;
use selium_protocol::utils::encode_message_batch;
use selium_protocol::{BiStream, Frame, PublisherPayload};
use selium_std::traits::codec::MessageEncoder;
use selium_std::traits::compression::Compress;
use std::marker::PhantomData;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Instant;

type Comp = Arc<dyn Compress + Send + Sync>;

#[doc(hidden)]
#[derive(Debug)]
pub struct PublisherWantsEncoder {
    pub(crate) common: StreamCommon,
}

#[doc(hidden)]
pub struct PublisherWantsOpen<E, Item> {
    common: StreamCommon,
    encoder: E,
    compression: Option<Comp>,
    batch_config: Option<BatchConfig>,
    _marker: PhantomData<Item>,
}

impl StreamBuilder<PublisherWantsEncoder> {
    /// Specifies the encoder a [Publisher](crate::Publisher) uses for encoding produced messages prior
    /// to being sent over the wire.
    ///
    /// An encoder can be any type implementing
    /// [MessageEncoder](crate::std::traits::codec::MessageEncoder).
    pub fn with_encoder<E, Item>(self, encoder: E) -> StreamBuilder<PublisherWantsOpen<E, Item>> {
        let state = PublisherWantsOpen {
            common: self.state.common,
            encoder,
            compression: None,
            batch_config: None,
            _marker: PhantomData,
        };

        StreamBuilder {
            state,
            connection: self.connection,
        }
    }
}

impl<E, Item> StreamBuilder<PublisherWantsOpen<E, Item>> {
    /// Specifies the compression implementation a [Publisher](crate::Publisher) uses for
    /// compressing encoded messages prior being sent over the wire.
    ///
    /// If message batching is enabled for the stream, the message batch will be compressed as a
    /// single unit, rather than each message being compressed individually.
    ///
    /// A compressor can be any type implementing [Compress](crate::std::traits::compression::Compress).
    pub fn with_compression<T>(mut self, comp: T) -> StreamBuilder<PublisherWantsOpen<E, Item>>
    where
        T: Compress + Send + Sync + 'static,
    {
        self.state.compression = Some(Arc::new(comp));
        self
    }

    /// Enables message batching for a [Publisher](crate::Publisher) stream.
    ///
    /// Relies on the specified [BatchConfig](crate::batching::BatchConfig) to tune the batching
    /// algorithm.
    ///
    /// When opted in for a stream, batching will happen automatically without any
    /// additional intervention.
    pub fn with_batching(
        mut self,
        config: BatchConfig,
    ) -> StreamBuilder<PublisherWantsOpen<E, Item>> {
        self.state.batch_config = Some(config);
        self
    }
}

impl<E, Item> Retain for StreamBuilder<PublisherWantsOpen<E, Item>> {
    fn retain<T: TryIntoU64>(mut self, policy: T) -> Result<Self> {
        self.state.common.retain(policy)?;
        Ok(self)
    }
}

impl<E, Item> Operations for StreamBuilder<PublisherWantsOpen<E, Item>> {
    fn map(mut self, module_path: &str) -> Self {
        self.state.common.map(module_path);
        self
    }

    fn filter(mut self, module_path: &str) -> Self {
        self.state.common.filter(module_path);
        self
    }
}

#[async_trait]
impl<E, Item> Open for StreamBuilder<PublisherWantsOpen<E, Item>>
where
    E: MessageEncoder<Item> + Clone + Send + Unpin,
    Item: Unpin + Send,
{
    type Output = Publisher<E, Item>;

    async fn open(self) -> Result<Self::Output> {
        let headers = PublisherPayload {
            topic: self.state.common.topic,
            retention_policy: self.state.common.retention_policy,
            operations: self.state.common.operations,
        };

        let publisher = Publisher::spawn(
            self.connection,
            headers,
            self.state.encoder,
            self.state.compression,
            self.state.batch_config,
        )
        .await?;

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
    compression: Option<Comp>,
    batch: Option<MessageBatch>,
    batch_config: Option<BatchConfig>,
    _marker: PhantomData<Item>,
}

impl<E, Item> Publisher<E, Item>
where
    E: MessageEncoder<Item> + Clone + Send + Unpin,
    Item: Unpin,
{
    async fn spawn(
        connection: Connection,
        headers: PublisherPayload,
        encoder: E,
        compression: Option<Comp>,
        batch_config: Option<BatchConfig>,
    ) -> Result<Self> {
        let mut stream = BiStream::try_from_connection(&connection).await?;
        let batch = batch_config.as_ref().map(|c| MessageBatch::from(c.clone()));
        let frame = Frame::RegisterPublisher(headers.clone());

        stream.send(frame).await?;

        Ok(Self {
            connection,
            stream,
            headers,
            encoder,
            compression,
            batch,
            batch_config,

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
            self.compression.clone(),
            self.batch_config.clone(),
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
        self.flush_batch()?;
        self.stream.finish().await
    }

    fn send_single(&mut self, mut bytes: Bytes) -> Result<()> {
        if let Some(comp) = &self.compression {
            bytes = comp.compress(bytes)?;
        }

        self.stream.start_send_unpin(Frame::Message(bytes))
    }

    fn send_batch(&mut self, now: Instant) -> Result<()> {
        let batch = self.batch.as_mut().unwrap();

        let messages = batch.drain();
        let mut bytes = encode_message_batch(messages);

        if let Some(comp) = &self.compression {
            bytes = comp.compress(bytes)?;
        }

        let frame = Frame::BatchMessage(bytes);
        self.stream.start_send_unpin(frame)?;
        batch.update_last_run(now);

        Ok(())
    }

    fn flush_batch(&mut self) -> Result<()> {
        if let Some(batch) = self.batch.as_ref() {
            if !batch.is_empty() {
                self.send_batch(Instant::now())?;
            }
        }

        Ok(())
    }
}

impl<E, Item> Sink<Item> for Publisher<E, Item>
where
    E: MessageEncoder<Item> + Clone + Send + Unpin,
    Item: Unpin,
{
    type Error = anyhow::Error;

    fn poll_ready(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<()>> {
        if let Some(batch) = self.batch.as_ref() {
            let now = Instant::now();

            if batch.is_ready(now) {
                self.send_batch(now)?;
            }

            return Poll::Ready(Ok(()));
        }

        self.stream.poll_ready_unpin(cx)
    }

    fn start_send(mut self: Pin<&mut Self>, item: Item) -> Result<()> {
        let bytes = self.encoder.encode(item)?;

        if let Some(batch) = self.batch.as_mut() {
            batch.push(bytes);
            Ok(())
        } else {
            self.send_single(bytes)
        }
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<()>> {
        self.stream.poll_flush_unpin(cx)
    }

    fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<()>> {
        self.stream.poll_close_unpin(cx)
    }
}
