use crate::traits::{Open, Operations, Retain, TryIntoU64};
use crate::{StreamBuilder, StreamCommon};
use anyhow::Result;
use async_trait::async_trait;
use bytes::{Bytes, BytesMut};
use futures::{SinkExt, Stream, StreamExt};
use quinn::Connection;
use selium_common::protocol::{decode_message_batch, Frame, SubscriberPayload};
use selium_common::types::BiStream;
use selium_std::traits::codec::MessageDecoder;
use selium_std::traits::compression::Decompress;
use std::marker::PhantomData;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

type Decomp = Arc<dyn Decompress + Send + Sync>;

#[doc(hidden)]
pub struct SubscriberWantsDecoder {
    pub(crate) common: StreamCommon,
}

#[doc(hidden)]
pub struct SubscriberWantsOpen<D, Item> {
    common: StreamCommon,
    decoder: D,
    decompression: Option<Decomp>,
    _marker: PhantomData<Item>,
}

impl StreamBuilder<SubscriberWantsDecoder> {
    /// Specifies the decoder a [Subscriber](crate::Subscriber) uses for decoding messages
    /// received over the wire.
    ///
    /// A decoder can be any type implementing
    /// [MessageDecoder](crate::traits::MessageDecoder). See [codecs](crate::codecs) for a list of
    /// codecs available in `Selium`, along with tutorials for creating your own decoders.
    pub fn with_decoder<D, Item>(self, decoder: D) -> StreamBuilder<SubscriberWantsOpen<D, Item>> {
        let state = SubscriberWantsOpen {
            common: self.state.common,
            decoder,
            decompression: None,
            _marker: PhantomData,
        };

        StreamBuilder {
            state,
            connection: self.connection,
        }
    }
}

impl<D, Item> StreamBuilder<SubscriberWantsOpen<D, Item>> {
    pub fn with_decompression<T>(mut self, decomp: T) -> StreamBuilder<SubscriberWantsOpen<D, Item>>
    where
        T: Decompress + Send + Sync + 'static,
    {
        self.state.decompression = Some(Arc::new(decomp));
        self
    }
}

impl<D, Item> Retain for StreamBuilder<SubscriberWantsOpen<D, Item>> {
    fn retain<T: TryIntoU64>(mut self, policy: T) -> Result<Self> {
        self.state.common.retain(policy)?;
        Ok(self)
    }
}

impl<D, Item> Operations for StreamBuilder<SubscriberWantsOpen<D, Item>> {
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
impl<D, Item> Open for StreamBuilder<SubscriberWantsOpen<D, Item>>
where
    D: MessageDecoder<Item> + Send,
    Item: Send,
{
    type Output = Subscriber<D, Item>;

    async fn open(self) -> Result<Self::Output> {
        let headers = SubscriberPayload {
            topic: self.state.common.topic,
            retention_policy: self.state.common.retention_policy,
            operations: self.state.common.operations,
        };

        let subscriber = Subscriber::spawn(
            self.connection,
            headers,
            self.state.decoder,
            self.state.decompression,
        )
        .await?;

        Ok(subscriber)
    }
}

/// A traditional subscriber stream that consumes messages produced by a topic.
///
/// The Subscriber struct implements the [futures::Stream] trait, and can thus be used in the same
/// contexts as a [Stream](futures::Stream). Any messages polled on the stream will be decoded
/// using the provided decoder.
///
/// **Note:** The Subscriber struct is never constructed directly, but rather, via a
/// [StreamBuilder](crate::StreamBuilder).
pub struct Subscriber<D, Item> {
    stream: BiStream,
    decoder: D,
    decompression: Option<Decomp>,
    message_batch: Option<Vec<Bytes>>,
    _marker: PhantomData<Item>,
}

impl<D, Item> Subscriber<D, Item>
where
    D: MessageDecoder<Item>,
{
    async fn spawn(
        connection: Connection,
        headers: SubscriberPayload,
        decoder: D,
        decompression: Option<Decomp>,
    ) -> Result<Self> {
        let mut stream = BiStream::try_from_connection(&connection).await?;
        let frame = Frame::RegisterSubscriber(headers);

        stream.send(frame).await?;
        stream.finish().await?;

        Ok(Self {
            stream,
            decoder,
            message_batch: None,
            decompression,
            _marker: PhantomData,
        })
    }

    fn decode_message(&mut self, bytes: Bytes) -> Poll<Option<Result<Item>>> {
        let mut mut_bytes = BytesMut::with_capacity(bytes.len());
        mut_bytes.extend_from_slice(&bytes);

        let decoded = self.decoder.decode(&mut mut_bytes)?;
        Poll::Ready(Some(Ok(decoded)))
    }
}

impl<D, Item> Stream for Subscriber<D, Item>
where
    D: MessageDecoder<Item> + Send + Unpin,
    Item: Unpin,
{
    type Item = Result<Item>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        // Attempt to pop a message off of the current batch, if available.
        if let Some(bytes) = self.message_batch.as_mut().and_then(|b| b.pop()) {
            return self.decode_message(bytes);
        }

        // Otherwise, poll a new frame from the stream
        let frame = match futures::ready!(self.stream.poll_next_unpin(cx)) {
            Some(Ok(frame)) => frame,
            Some(Err(err)) => return Poll::Ready(Some(Err(err))),
            None => return Poll::Ready(None),
        };

        match frame {
            // If the frame is a standard, unbatched message, then decode and return it
            // immediately.
            Frame::Message(mut bytes) => {
                if let Some(decomp) = &self.decompression {
                    bytes = decomp.decompress(bytes)?;
                }

                self.decode_message(bytes)
            }
            // If the frame is a batched message, then set the current batch and call `poll_next`
            // again to begin popping off messages.
            Frame::BatchMessage(mut bytes) => {
                if let Some(decomp) = &self.decompression {
                    bytes = decomp.decompress(bytes)?;
                }

                let batch = decode_message_batch(bytes);
                self.message_batch = Some(batch);
                self.poll_next(cx)
            }
            // Otherwise, do nothing.
            _ => Poll::Ready(None),
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.stream.size_hint()
    }
}
