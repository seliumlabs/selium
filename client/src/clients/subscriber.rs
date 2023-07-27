use super::builder::{ClientBuilder, ClientCommon};
use crate::crypto::cert::load_root_store;
use crate::protocol::{Frame, SubscriberPayload};
use crate::traits::{Client, ClientConfig, Connect, TryIntoU64, MessageDecoder};
use crate::utils::client::establish_connection;
use crate::BiStream;
use anyhow::Result;
use async_trait::async_trait;
use bytes::BytesMut;
use futures::{SinkExt, Stream, StreamExt};
use rustls::RootCertStore;
use std::marker::PhantomData;
use std::path::PathBuf;
use std::pin::Pin;
use std::task::{Context, Poll};

#[derive(Debug)]
pub struct SubscriberWantsCert {
    common: ClientCommon,
}

#[derive(Debug)]
pub struct SubscriberWantsDecoder {
    common: ClientCommon,
    root_store: RootCertStore,
}

#[derive(Debug)]
pub struct SubscriberReady<E, Item> {
    common: ClientCommon,
    root_store: RootCertStore,
    decoder: E,
    _marker: PhantomData<Item>,
}

pub fn subscriber(topic: &str) -> ClientBuilder<SubscriberWantsCert> {
    ClientBuilder {
        state: SubscriberWantsCert {
            common: ClientCommon::new(topic),
        },
    }
}

impl ClientConfig for ClientBuilder<SubscriberWantsCert> {
    type NextState = ClientBuilder<SubscriberWantsDecoder>;

    fn map(mut self, module_path: &str) -> Self {
        self.state.common.map(module_path);
        self
    }

    fn filter(mut self, module_path: &str) -> Self {
        self.state.common.map(module_path);
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

        let state = SubscriberWantsDecoder {
            common: self.state.common,
            root_store,
        };

        Ok(ClientBuilder { state })
    }
}

impl ClientBuilder<SubscriberWantsDecoder> {
    pub fn with_decoder<D, Item>(self, decoder: D) -> ClientBuilder<SubscriberReady<D, Item>> {
        let state = SubscriberReady {
            common: self.state.common,
            root_store: self.state.root_store,
            decoder,
            _marker: PhantomData,
        };

        ClientBuilder { state }
    }
}

#[async_trait]
impl<D, Item> Connect for ClientBuilder<SubscriberReady<D, Item>>
where
    D: MessageDecoder<Item> + Send,
    Item: Send,
{
    type Output = Subscriber<D, Item>;

    async fn connect(self, host: &str) -> Result<Self::Output> {
        let mut stream =
            establish_connection(host, &self.state.root_store, self.state.common.keep_alive)
                .await?;

        let frame = Frame::RegisterSubscriber(SubscriberPayload {
            topic: self.state.common.topic,
            retention_policy: self.state.common.retention_policy,
            operations: self.state.common.operations,
        });

        stream.send(frame).await?;

        Ok(Subscriber {
            stream,
            decoder: self.state.decoder,
            _marker: PhantomData,
        })
    }
}

pub struct Subscriber<D, Item> {
    stream: BiStream,
    decoder: D,
    _marker: PhantomData<Item>,
}

#[async_trait]
impl<D, Item> Client for Subscriber<D, Item>
where
    D: MessageDecoder<Item> + Send,
    Item: Send,
{
    async fn finish(self) -> Result<()> {
        self.stream.finish().await
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
