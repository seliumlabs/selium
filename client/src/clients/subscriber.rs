use super::builder::{ClientBuilder, ClientCommon};
use crate::aliases::Streams;
use crate::crypto::cert::load_root_store;
use crate::protocol::{Frame, SubscriberPayload};
use crate::traits::{Client, ClientConfig, Connect, IntoTimestamp};
use crate::utils::client::establish_connection;
use anyhow::Result;
use async_trait::async_trait;
use futures::{SinkExt, Stream, StreamExt};
use rustls::RootCertStore;
use std::path::PathBuf;
use std::pin::Pin;
use std::task::{Context, Poll};

#[derive(Debug)]
pub struct SubscriberWantsCert {
    common: ClientCommon,
}

#[derive(Debug)]
pub struct SubscriberHasCert {
    common: ClientCommon,
    root_store: RootCertStore,
}

pub fn subscriber(topic: &str) -> ClientBuilder<SubscriberWantsCert> {
    ClientBuilder {
        state: SubscriberWantsCert {
            common: ClientCommon::new(topic),
        },
    }
}

impl ClientConfig for ClientBuilder<SubscriberWantsCert> {
    type NextState = ClientBuilder<SubscriberHasCert>;

    fn map(mut self, module_path: &str) -> Self {
        self.state.common.map(module_path);
        self
    }

    fn filter(mut self, module_path: &str) -> Self {
        self.state.common.map(module_path);
        self
    }

    fn keep_alive<T: IntoTimestamp>(mut self, interval: T) -> Result<Self> {
        self.state.common.keep_alive(interval)?;
        Ok(self)
    }

    fn with_certificate_authority<T: Into<PathBuf>>(self, ca_path: T) -> Result<Self::NextState> {
        let root_store = load_root_store(&ca_path.into())?;

        let state = SubscriberHasCert {
            common: self.state.common,
            root_store,
        };

        Ok(ClientBuilder { state })
    }
}

#[async_trait]
impl Connect for ClientBuilder<SubscriberHasCert> {
    type Output = Subscriber;

    async fn register(self, streams: &mut Streams) -> Result<()> {
        let (ref mut write, _) = streams;

        let frame = Frame::RegisterSubscriber(SubscriberPayload {
            topic: self.state.common.topic,
            operations: self.state.common.operations,
        });

        write.send(frame).await?;

        Ok(())
    }

    async fn connect(self, host: &str) -> Result<Self::Output> {
        let mut streams =
            establish_connection(host, &self.state.root_store, self.state.common.keep_alive)
                .await?;

        self.register(&mut streams).await?;

        Ok(Subscriber { streams })
    }
}

pub struct Subscriber {
    streams: Streams,
}

#[async_trait]
impl Client for Subscriber {
    async fn finish(self) -> Result<()> {
        let (write, _) = self.streams;

        write.into_inner().finish().await?;

        Ok(())
    }
}

impl Stream for Subscriber {
    type Item = Result<String>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let frame = match futures::ready!(self.streams.1.poll_next_unpin(cx)) {
            Some(Ok(frame)) => frame,
            Some(Err(err)) => return Poll::Ready(Some(Err(err))),
            None => return Poll::Ready(None),
        };

        match frame {
            Frame::Message(inner_string) => Poll::Ready(Some(Ok(inner_string))),
            _ => Poll::Ready(None),
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.streams.1.size_hint()
    }
}
