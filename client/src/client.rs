use crate::crypto::cert::load_root_store;
use crate::traits::TryIntoU64;
use crate::utils::client::establish_connection;
use crate::{PublisherWantsEncoder, StreamBuilder, StreamCommon, SubscriberWantsDecoder};
use anyhow::Result;
use quinn::Connection;
use rustls::RootCertStore;
use std::path::PathBuf;

pub const KEEP_ALIVE_DEFAULT: u64 = 5;

#[derive(Debug)]
pub struct ClientWantsCert {
    keep_alive: u64,
}

#[derive(Debug)]
pub struct ClientWantsConnect {
    keep_alive: u64,
    root_store: RootCertStore,
}

#[derive(Debug)]
pub struct ClientBuilder<T> {
    pub state: T,
}

pub fn client() -> ClientBuilder<ClientWantsCert> {
    ClientBuilder {
        state: ClientWantsCert {
            keep_alive: KEEP_ALIVE_DEFAULT,
        },
    }
}

impl ClientBuilder<ClientWantsCert> {
    pub fn keep_alive<T: TryIntoU64>(mut self, interval: T) -> Result<Self> {
        self.state.keep_alive = interval.try_into_u64()?;
        Ok(self)
    }

    pub fn with_certificate_authority<T: Into<PathBuf>>(
        self,
        ca_path: T,
    ) -> Result<ClientBuilder<ClientWantsConnect>> {
        let root_store = load_root_store(&ca_path.into())?;

        let state = ClientWantsConnect {
            keep_alive: self.state.keep_alive,
            root_store,
        };

        Ok(ClientBuilder { state })
    }
}

impl ClientBuilder<ClientWantsConnect> {
    pub async fn connect(self, host: &str) -> Result<Client> {
        let connection =
            establish_connection(host, &self.state.root_store, self.state.keep_alive).await?;

        Ok(Client { connection })
    }
}

pub struct Client {
    connection: Connection,
}

impl Client {
    pub fn subscriber(&self, topic: &str) -> StreamBuilder<SubscriberWantsDecoder> {
        StreamBuilder {
            connection: self.connection.clone(),
            state: SubscriberWantsDecoder {
                common: StreamCommon::new(topic),
            },
        }
    }

    pub fn publisher(&self, topic: &str) -> StreamBuilder<PublisherWantsEncoder> {
        StreamBuilder {
            connection: self.connection.clone(),
            state: PublisherWantsEncoder {
                common: StreamCommon::new(topic),
            },
        }
    }
}
