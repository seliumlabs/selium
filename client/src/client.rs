use crate::crypto::cert::{load_keypair, load_root_store};
use crate::traits::TryIntoU64;
use crate::utils::client::establish_connection;
use crate::{PublisherWantsEncoder, StreamBuilder, StreamCommon, SubscriberWantsDecoder};
use anyhow::Result;
use quinn::{Connection, VarInt};
use rustls::{Certificate, PrivateKey, RootCertStore};
use std::path::Path;

/// The default `keep_alive` interval for a client connection.
pub const KEEP_ALIVE_DEFAULT: u64 = 5_000;

#[doc(hidden)]
#[derive(Debug)]
pub struct ClientWantsRootCert {
    keep_alive: u64,
}

#[derive(Debug)]
pub struct ClientWantsCertAndKey {
    keep_alive: u64,
    root_store: RootCertStore,
}

#[doc(hidden)]
#[derive(Debug)]
pub struct ClientWantsConnect {
    keep_alive: u64,
    root_store: RootCertStore,
    certs: Vec<Certificate>,
    key: PrivateKey,
}

/// A convenient builder struct used to build a [Client] instance.
///
/// The [ClientBuilder] uses a type-level Finite State Machine to assure that a [Client] cannot
/// be constructed with an invalid state. For example, the [connect](ClientBuilder::connect) method
/// will not be in-scope unless the [ClientBuilder] is in a pre-connection state, which is achieved
/// by first configuring the root store.
///
/// **NOTE:** The [ClientBuilder] type is not intended to be used directly. Use the [client]
/// function to construct a [ClientBuilder] in its initial state.
#[derive(Debug)]
pub struct ClientBuilder<T> {
    state: T,
}

/// Constructs a [ClientBuilder] in its initial state. Prefer invoking this function over
/// explicitly constructing a [ClientBuilder].
///
/// ```
/// let client = selium::client();
/// ```
pub fn client() -> ClientBuilder<ClientWantsRootCert> {
    ClientBuilder {
        state: ClientWantsRootCert {
            keep_alive: KEEP_ALIVE_DEFAULT,
        },
    }
}

impl ClientBuilder<ClientWantsRootCert> {
    /// Overrides the `keep_alive` interval for the client connection in milliseconds.
    ///
    /// Accepts any `interval` argument that can be *fallibly* converted into a [u64] via the
    /// [TryIntoU64](crate::traits::TryIntoU64) trait.
    ///
    /// **NOTE:** `Selium` already provides a reasonable default for the `keep_alive` interval (see
    /// [KEEP_ALIVE_DEFAULT]), so this setting should only be overridden if it's not suitable for
    /// your use-case.
    ///
    /// # Errors
    ///
    /// Returns [Err] if the provided interval fails to be convert to a [u64].
    ///
    /// # Examples
    ///
    /// Overriding the default `keep_alive` interval as 6 seconds (represented in milliseconds).
    ///  
    /// ```
    /// let client = selium::client()
    ///     .keep_alive(6_000).unwrap();
    /// ```
    ///
    /// You can even use any other type that can be converted to a [u64] via the
    /// [TryIntoU64](crate::traits::TryIntoU64) trait, such as the standard library's Duration
    /// type.
    ///
    /// ```
    /// use std::time::Duration;
    ///
    /// let client = selium::client()
    ///     .keep_alive(Duration::from_secs(6)).unwrap();
    /// ```
    pub fn keep_alive<T: TryIntoU64>(mut self, interval: T) -> Result<Self> {
        self.state.keep_alive = interval.try_into_u64()?;
        Ok(self)
    }

    /// Attempts to load a valid CA certificate from the filesystem, and creates a root cert store
    /// to use with authenticating the QUIC connection.
    ///
    /// Following this method, the [ClientBuilder] will be in a pre-connection state, so any
    /// additional configuration must take place before invoking this method.
    ///
    /// # Errors
    ///
    /// Returns [Err] if the provided `ca_path` argument does not refer to a file containing a
    /// valid certificate.
    pub fn with_certificate_authority<T: AsRef<Path>>(
        self,
        ca_path: T,
    ) -> Result<ClientBuilder<ClientWantsCertAndKey>> {
        let root_store = load_root_store(ca_path)?;
        let state = ClientWantsCertAndKey {
            keep_alive: self.state.keep_alive,
            root_store,
        };
        Ok(ClientBuilder { state })
    }
}

impl ClientBuilder<ClientWantsCertAndKey> {
    pub fn with_cert_and_key<T: AsRef<Path>>(
        self,
        cert_file: T,
        key_file: T,
    ) -> Result<ClientBuilder<ClientWantsConnect>> {
        let (certs, key) = load_keypair(cert_file, key_file)?;
        let state = ClientWantsConnect {
            keep_alive: self.state.keep_alive,
            root_store: self.state.root_store,
            certs,
            key,
        };
        Ok(ClientBuilder { state })
    }
}

impl ClientBuilder<ClientWantsConnect> {
    /// Attempts to establish a connection with the `Selium` server corresponding to the provided
    /// `addr` argument. The [connect](ClientBuilder::connect) method will only be in scope if the
    /// [ClientBuilder] is in a pre-connect state, `ClientWantsConnect`.
    ///
    /// # Errors
    ///
    /// Returns [Err] under the following conditions:
    ///
    /// - If the provided `addr` argument does not resolve to a valid
    /// [SocketAddr](std::net::SocketAddr).
    /// - If the connection cannot be established.
    pub async fn connect(self, addr: &str) -> Result<Client> {
        let connection = establish_connection(
            addr,
            self.state.certs,
            self.state.key,
            &self.state.root_store,
            self.state.keep_alive,
        )
        .await?;

        tokio::spawn({
            let connection = connection.clone();
            async move {
                tokio::signal::ctrl_c().await.unwrap();
                connection.close(VarInt::from_u32(0), b"Client forcefully closed connection");
            }
        });

        Ok(Client { connection })
    }
}

/// A client containing an authenticated connection to the `Selium` server.
///
/// The [Client] struct is the entry point to opening various `Selium` streams, such as the
/// [Publisher](crate::Publisher) and [Subscriber](crate::Subscriber) stream types.
///
/// Multiple streams can be opened from a single connected [Client] without extinguishing the underlying
/// connection, through the use of [QUIC](https://quicwg.org) multiplexing.
///
/// **NOTE:** The [Client] struct should never be used directly, and is intended to be constructed by a
/// [ClientBuilder], following a successfully established connection to the `Selium` server.
#[derive(Clone)]
pub struct Client {
    connection: Connection,
}

impl Client {
    /// Returns a new [StreamBuilder](crate::StreamBuilder) instance, with an initial `Subscriber`
    /// state.
    pub fn subscriber(&self, topic: &str) -> StreamBuilder<SubscriberWantsDecoder> {
        StreamBuilder {
            connection: self.connection.clone(),
            state: SubscriberWantsDecoder {
                common: StreamCommon::new(topic),
            },
        }
    }

    /// Returns a new [StreamBuilder](crate::StreamBuilder) instance, with an initial `Publisher`
    /// state.
    pub fn publisher(&self, topic: &str) -> StreamBuilder<PublisherWantsEncoder> {
        StreamBuilder {
            connection: self.connection.clone(),
            state: PublisherWantsEncoder {
                common: StreamCommon::new(topic),
            },
        }
    }
}
