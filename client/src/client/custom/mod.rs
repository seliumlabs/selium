mod states;
pub use states::*;

use crate::connection::{ClientConnection, ConnectionOptions};
use crate::constants::SELIUM_CLOUD_REMOTE_URL;
use crate::crypto::cert::{load_certs, load_keypair, load_root_store};
use crate::keep_alive::BackoffStrategy;
use crate::traits::TryIntoU64;
use crate::{Client, ClientBuilder, ClientCommon};
use selium_std::errors::{Result, SeliumError};
use std::path::Path;
use std::sync::Arc;
use tokio::sync::Mutex;

impl ClientBuilder<CustomWantsEndpoint> {
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

    /// Specifies the remote server address to connect to.
    pub fn endpoint(self, endpoint: &str) -> ClientBuilder<CustomWantsRootCert> {
        let next_state = CustomWantsRootCert::new(self.state, endpoint);
        ClientBuilder { state: next_state }
    }
}

impl ClientBuilder<CustomWantsRootCert> {
    /// Attempts to load a valid CA certificate from the filesystem, and creates a root cert store
    /// to use with authenticating the QUIC connection.
    ///
    /// Certificates can be encoded in either a Base64 ASCII (.pem) or binary (.der) format.
    ///
    /// # Errors
    ///
    /// Returns [Err] if the provided `ca_path` argument does not refer to a file containing a
    /// valid certificate.
    pub fn with_certificate_authority<T: AsRef<Path>>(
        self,
        ca_path: T,
    ) -> Result<ClientBuilder<CustomWantsCertAndKey>> {
        let ca_certs = load_certs(ca_path)?;
        let root_store = load_root_store(&ca_certs)?;
        let next_state = CustomWantsCertAndKey::new(self.state, root_store);
        Ok(ClientBuilder { state: next_state })
    }
}

impl ClientBuilder<CustomWantsCertAndKey> {
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
    ) -> Result<ClientBuilder<CustomWantsConnect>> {
        let (certs, key) = load_keypair(cert_file, key_file)?;
        let next_state = CustomWantsConnect::new(self.state, &certs, key);
        Ok(ClientBuilder { state: next_state })
    }
}

impl ClientBuilder<CustomWantsConnect> {
    /// Attempts to establish a connection with the `Selium` server corresponding to the provided
    /// `addr` argument. The [connect](ClientBuilder::connect) method will only be in scope if the
    /// [ClientBuilder] is in a pre-connect state, `CustomWantsConnect`.
    ///
    /// # Errors
    ///
    /// Returns [Err] under the following conditions:
    ///
    /// - If the provided `addr` argument does not resolve to a valid
    /// [SocketAddr](std::net::SocketAddr).
    /// - If the connection cannot be established.
    pub async fn connect(self) -> Result<Client> {
        let CustomWantsConnect {
            common,
            certs,
            key,
            endpoint,
            root_store,
        } = self.state;
        if endpoint == SELIUM_CLOUD_REMOTE_URL {
            return Err(SeliumError::ConnectDirectToCloud)
        }

        let ClientCommon {
            keep_alive,
            backoff_strategy,
        } = common;

        let options = ConnectionOptions::new(certs.as_slice(), key, root_store, keep_alive);
        let connection = ClientConnection::connect(&endpoint, options).await?;
        let connection = Arc::new(Mutex::new(connection));

        Ok(Client {
            connection,
            backoff_strategy,
        })
    }
}
