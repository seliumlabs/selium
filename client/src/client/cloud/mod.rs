mod states;
pub use states::*;

use crate::connection::{ClientConnection, ConnectionOptions};
use crate::constants::SELIUM_CLOUD_REMOTE_URL;
use crate::crypto::cert::load_keypair;
use crate::keep_alive::BackoffStrategy;
use crate::traits::TryIntoU64;
use crate::logging;
use crate::{Client, ClientBuilder, ClientCommon};
use selium_std::errors::{Result, SeliumError};
use std::path::Path;
use std::sync::Arc;
use tokio::sync::Mutex;

impl ClientBuilder<CloudWantsCertAndKey> {
    /// See [keep_alive](ClientCommon::keep_alive) in [ClientCommon].
    pub fn keep_alive<T: TryIntoU64>(mut self, interval: T) -> Result<Self> {
        self.state.common.keep_alive(interval)?;
        Ok(self)
    }

    /// See [backoff_strategy](ClientCommon::backoff_strategy) in [ClientCommon].
    pub fn backoff_strategy(mut self, strategy: BackoffStrategy) -> Self {
        self.state.common.backoff_strategy(strategy);
        self
    }

    /// Attempts to load a valid keypair from the filesystem to use with authenticating the QUIC connection.
    ///
    /// Keypairs can be encoded in either a Base64 ASCII (.pem) or binary (.der) format.
    ///
    /// Following this method, the [ClientBuilder] will be in a pre-connection state, so any
    /// additional configuration must take place before invoking this method.
    ///
    /// # Errors
    ///
    /// Returns [Err] under the following conditions:
    ///
    /// - The provided `cert_file` argument does not refer to a file containing a
    /// valid certificate.
    ///
    /// - The provided `key_file` argument does not refer to a file containing a
    /// valid PKCS-8 private key.
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
    /// Attempts to establish a connection with `Selium Cloud`.
    ///
    /// The [connect](ClientBuilder::connect) method will only be in scope if the
    /// [ClientBuilder] is in a pre-connect state, `CloudWantsConnect`.
    ///
    /// # Errors
    ///
    /// Returns [Err] under the following conditions:
    ///
    /// - If the connection cannot be established.
    pub async fn connect(self) -> Result<Client> {
        let CloudWantsConnect {
            common,
            certs,
            key,
            root_store,
        } = self.state;
        let ClientCommon {
            keep_alive,
            backoff_strategy,
        } = common;

        let options = ConnectionOptions::new(certs.as_slice(), key, root_store, keep_alive);
        logging::connection::get_cloud_endpoint();
        let endpoint = get_cloud_endpoint(options.clone()).await?;

        logging::connection::connect_to_address(&endpoint);
        let connection = ClientConnection::connect(&endpoint, options).await?;
        let connection = Arc::new(Mutex::new(connection));
        logging::connection::successful_connection(&endpoint);

        Ok(Client {
            connection,
            backoff_strategy,
        })
    }
}

#[tracing::instrument]
async fn get_cloud_endpoint(options: ConnectionOptions) -> Result<String> {
    let connection = ClientConnection::connect(SELIUM_CLOUD_REMOTE_URL, options).await?;
    let (_, mut read) = connection
        .conn()
        .open_bi()
        .await
        .map_err(SeliumError::OpenCloudStreamFailed)?;
    let endpoint_bytes = read
        .read_to_end(2048)
        .await
        .map_err(|_| SeliumError::GetServerAddressFailed)?;
    let endpoint =
        String::from_utf8(endpoint_bytes).map_err(|_| SeliumError::GetServerAddressFailed)?;

    Ok(endpoint)
}
