mod states;
pub use states::*;

use crate::connection::{ClientConnection, ConnectionOptions, configure_client, get_cloud_endpoint};
use crate::constants::SELIUM_CLOUD_REMOTE_URL;
use crate::crypto::cert::load_keypair;
use crate::keep_alive::BackoffStrategy;
use crate::logging;
use crate::traits::TryIntoU64;
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
        Ok(ClientBuilder { state: next_state, client_type: self.client_type })
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
        let client_config = configure_client(options).await; 
        let endpoint = get_cloud_endpoint(client_config.clone()).await?;

        logging::connection::connect_to_address(&endpoint);
        let connection = ClientConnection::connect(&endpoint, client_config).await?;
        let connection = Arc::new(Mutex::new(connection));
        logging::connection::successful_connection(&endpoint);

        Ok(Client {
            connection,
            backoff_strategy,
        })
    }
}
