use crate::aliases::Streams;
use crate::crypto::cert::load_root_store;
use crate::protocol::{Frame, SubscriberPayload};
use crate::traits::{ClientAuth, Connect, Operations};
use crate::utils::client::{configure_client, get_client_connection, get_client_streams};
use crate::utils::net::get_socket_addrs;
use crate::Operation;
use anyhow::Result;
use async_trait::async_trait;
use futures::{future, SinkExt, StreamExt};
use rustls::RootCertStore;
use std::path::PathBuf;

#[derive(Debug)]
pub struct WantsCert {
    topic: String,
    operations: Vec<Operation>,
}

#[derive(Debug)]
pub struct HasCert {
    topic: String,
    operations: Vec<Operation>,
    root_store: RootCertStore,
}

pub fn subscriber(topic: &str) -> SubscriberBuilder<WantsCert> {
    SubscriberBuilder {
        state: WantsCert {
            topic: topic.to_owned(),
            operations: Vec::new(),
        },
    }
}

pub struct SubscriberBuilder<T> {
    state: T,
}

impl Operations for SubscriberBuilder<WantsCert> {
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

impl ClientAuth for SubscriberBuilder<WantsCert> {
    type Output = SubscriberBuilder<HasCert>;

    fn with_certificate_authority<T: Into<PathBuf>>(self, ca_path: T) -> Result<Self::Output> {
        let root_store = load_root_store(&ca_path.into())?;

        let state = HasCert {
            topic: self.state.topic,
            operations: self.state.operations,
            root_store,
        };

        Ok(SubscriberBuilder { state })
    }
}

#[async_trait]
impl Connect for SubscriberBuilder<HasCert> {
    type Output = Subscriber;

    async fn connect(self, host: &str) -> Result<Self::Output> {
        let addr = get_socket_addrs(host)?;
        let config = configure_client(&self.state.root_store)?;
        let connection = get_client_connection(config, addr).await?;
        let mut streams = get_client_streams(connection).await?;

        register_subscriber(self, &mut streams).await?;

        Ok(Subscriber { streams })
    }
}

pub struct Subscriber {
    streams: Streams,
}

impl Subscriber {
    pub async fn subscribe(&mut self) -> Result<()> {
        let (_, ref mut read) = self.streams;

        read.for_each(|frame| future::ready(println!("{:?}", frame)))
            .await;

        Ok(())
    }

    pub async fn finish(self) -> Result<()> {
        let (write, _) = self.streams;

        write.into_inner().finish().await?;

        Ok(())
    }
}

pub async fn register_subscriber(
    client: SubscriberBuilder<HasCert>,
    streams: &mut Streams,
) -> Result<()> {
    let (ref mut write, _) = streams;

    let frame = Frame::RegisterSubscriber(SubscriberPayload {
        topic: client.state.topic,
        operations: client.state.operations,
    });

    write.send(frame).await?;

    Ok(())
}
