mod states;
pub use states::*;

use crate::connection::{ConnectionOptions, ClientConnection};
use crate::constants::SELIUM_CLOUD_REMOTE_URL;
use crate::crypto::cert::load_keypair;
use crate::keep_alive::BackoffStrategy;
use crate::traits::TryIntoU64;
use crate::{ClientBuilder, ClientCommon, Client};
use selium_std::errors::{Result, SeliumError};
use std::path::Path;
use std::sync::Arc;
use tokio::sync::Mutex;

impl ClientBuilder<CloudWantsCertAndKey> {
    pub fn keep_alive<T: TryIntoU64>(mut self, interval: T) -> Result<Self> {
        self.state.common.keep_alive(interval)?;
        Ok(self)
    }

    pub fn backoff_strategy(mut self, strategy: BackoffStrategy) -> Self {
        self.state.common.backoff_strategy(strategy);
        self
    }

    pub fn with_cert_and_key<T: AsRef<Path>>(
        self,
        cert_file: T,
        key_file: T,
    ) -> Result<ClientBuilder<CloudWantsConnect>> {
        let (certs, key) = load_keypair(cert_file, key_file)?;
        let next_state = CloudWantsConnect::new(self.state, &certs, key);
        Ok(ClientBuilder { state: next_state })
    }
}

impl ClientBuilder<CloudWantsConnect> {
    pub async fn connect(self) -> Result<Client> {
        let CloudWantsConnect { common, certs, key, root_store } = self.state;
        let ClientCommon { keep_alive, backoff_strategy } = common;

        let options = ConnectionOptions::new(
            certs.as_slice(),
            key,
            root_store,
            keep_alive,
        );

        let endpoint = get_cloud_endpoint(options.clone()).await?;
        let connection = ClientConnection::connect(&endpoint, options).await?;
        let connection = Arc::new(Mutex::new(connection));

        Ok(Client {
            connection,
            backoff_strategy,
        })
    }
}

async fn get_cloud_endpoint(options: ConnectionOptions) -> Result<String> {
    let connection = ClientConnection::connect(SELIUM_CLOUD_REMOTE_URL, options).await?;
    let (_, mut read) = connection.conn().open_bi().await.map_err(SeliumError::OpenCloudStreamFailed)?;
    let endpoint_bytes = read.read_to_end(2048).await.map_err(|_| SeliumError::GetServerAddressFailed)?;
    let endpoint = String::from_utf8(endpoint_bytes).map_err(|_| SeliumError::GetServerAddressFailed)?;

    Ok(endpoint)
}
