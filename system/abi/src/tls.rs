//! TLS-related hostcall payloads for network configuration.

use rkyv::{Archive, Deserialize, Serialize};

use crate::GuestResourceId;

/// TLS material supplied by a guest for server listeners.
#[derive(Debug, Clone, PartialEq, Eq, Archive, Serialize, Deserialize)]
#[rkyv(bytecheck())]
pub struct TlsServerBundle {
    /// PEM-encoded certificate chain presented by the server.
    pub cert_chain_pem: Vec<u8>,
    /// PEM-encoded private key for the certificate chain.
    pub private_key_pem: Vec<u8>,
    /// PEM-encoded CA bundle used to verify client certificates.
    pub client_ca_pem: Option<Vec<u8>>,
    /// Optional ALPN protocol list.
    pub alpn: Option<Vec<String>>,
    /// Require client authentication when true.
    pub require_client_auth: bool,
}

/// TLS material supplied by a guest for client connections.
#[derive(Debug, Clone, PartialEq, Eq, Archive, Serialize, Deserialize)]
#[rkyv(bytecheck())]
pub struct TlsClientBundle {
    /// PEM-encoded CA bundle used to verify servers.
    pub ca_bundle_pem: Option<Vec<u8>>,
    /// PEM-encoded client certificate chain.
    pub client_cert_pem: Option<Vec<u8>>,
    /// PEM-encoded private key for the client certificate.
    pub client_key_pem: Option<Vec<u8>>,
    /// Optional ALPN protocol list.
    pub alpn: Option<Vec<String>>,
}

/// Arguments for creating a server-side TLS configuration handle.
#[derive(Debug, Clone, PartialEq, Eq, Archive, Serialize, Deserialize)]
#[rkyv(bytecheck())]
pub struct NetTlsServerConfig {
    /// TLS bundle supplied for server listeners.
    pub bundle: TlsServerBundle,
}

/// Arguments for creating a client-side TLS configuration handle.
#[derive(Debug, Clone, PartialEq, Eq, Archive, Serialize, Deserialize)]
#[rkyv(bytecheck())]
pub struct NetTlsClientConfig {
    /// TLS bundle supplied for client connections.
    pub bundle: TlsClientBundle,
}

/// Reply containing a TLS configuration handle.
#[derive(Debug, Clone, PartialEq, Eq, Archive, Serialize, Deserialize)]
#[rkyv(bytecheck())]
pub struct NetTlsConfigReply {
    /// TLS configuration handle registered in the instance registry.
    pub handle: GuestResourceId,
}
