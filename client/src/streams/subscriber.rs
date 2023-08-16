use crate::traits::{MessageDecoder, Open, Operations, Retain, TryIntoU64};
use crate::{StreamBuilder, StreamCommon};
use anyhow::Result;
use async_trait::async_trait;
use bytes::BytesMut;
use futures::{SinkExt, Stream, StreamExt};
use quinn::Connection;
use selium_common::protocol::{Frame, SubscriberPayload};
use selium_common::types::{BiStream, Executor};
use selium_common::traits::SeliumCodec;
use std::marker::PhantomData;
use std::pin::Pin;
use std::task::{Context, Poll};

#[doc(hidden)]
#[derive(Debug)]
pub struct SubscriberWantsDecoder {
    pub(crate) common: StreamCommon,
}

#[doc(hidden)]
#[derive(Debug)]
pub struct SubscriberWantsOpen<D, Item> {
    common: StreamCommon,
    decoder: D,
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
            _marker: PhantomData,
        };

        StreamBuilder {
            state,
            connection: self.connection,
        }
    }
}

impl<D, Item> Retain for StreamBuilder<SubscriberWantsOpen<D, Item>>
where
    D: MessageDecoder<Item>,
{
    fn retain<T: TryIntoU64>(mut self, policy: T) -> Result<Self> {
        self.state.common.retain(policy)?;
        Ok(self)
    }
}

impl<D, Item> Operations for StreamBuilder<SubscriberWantsOpen<D, Item>>
where
    D: MessageDecoder<Item> + SeliumCodec,
{
    fn map(mut self, executor: Executor) -> Self {
        println!("{executor:?}");
        self.state.common.map(executor);
        self
    }

    fn filter(mut self, executor: Executor) -> Self {
        self.state.common.filter(executor);
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

        let subscriber = Subscriber::spawn(self.connection, headers, self.state.decoder).await?;

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
    _marker: PhantomData<Item>,
}

impl<D, Item> Subscriber<D, Item>
where
    D: MessageDecoder<Item>,
{
    async fn spawn(connection: Connection, headers: SubscriberPayload, decoder: D) -> Result<Self> {
        let mut stream = BiStream::try_from_connection(&connection).await?;
        let frame = Frame::RegisterSubscriber(headers);

        stream.send(frame).await?;
        stream.finish().await?;

        Ok(Self {
            stream,
            decoder,
            _marker: PhantomData,
        })
    }
}

impl<D, Item> Stream for Subscriber<D, Item>
where
    D: MessageDecoder<Item> + Send + Unpin,
    Item: Unpin,
{
    type Item = Result<Item>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let frame = match futures::ready!(self.stream.poll_next_unpin(cx)) {
            Some(Ok(frame)) => frame,
            Some(Err(err)) => return Poll::Ready(Some(Err(err))),
            None => return Poll::Ready(None),
        };

        let bytes = match frame {
            Frame::Message(bytes) => bytes,
            _ => return Poll::Ready(None),
        };

        let mut mut_bytes = BytesMut::with_capacity(bytes.len());
        mut_bytes.extend_from_slice(&bytes[..]);

        Poll::Ready(Some(self.decoder.decode(&mut mut_bytes)))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.stream.size_hint()
    }
}
