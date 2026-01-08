use rkyv::{Archive, Deserialize, Serialize};

use crate::GuestResourceId;

/// Network transport protocols supported by the ABI.
#[repr(u8)]
#[derive(Debug, Copy, Clone, PartialEq, Eq, Archive, Serialize, Deserialize)]
#[rkyv(bytecheck())]
pub enum NetProtocol {
    /// QUIC over UDP with TLS 1.3.
    Quic = 0,
    /// HTTP/1.1 over TCP (cleartext).
    Http = 1,
    /// HTTP/2 over TLS 1.3.
    Https = 2,
}

/// Arguments for creating a network listener.
#[derive(Debug, Clone, PartialEq, Archive, Serialize, Deserialize)]
#[rkyv(bytecheck())]
pub struct NetCreateListener {
    /// Protocol to use for the listener.
    pub protocol: NetProtocol,
    /// Authority or hostname to bind to.
    pub domain: String,
    /// Port number to bind to.
    pub port: u16,
    /// Optional TLS configuration handle for QUIC/HTTPS listeners.
    pub tls: Option<GuestResourceId>,
}

/// Reply containing guest-visible handles for a created listener.
#[derive(Debug, Clone, PartialEq, Eq, Archive, Serialize, Deserialize)]
#[rkyv(bytecheck())]
pub struct NetCreateListenerReply {
    /// Listener handle registered in the instance registry.
    pub handle: GuestResourceId,
}

/// Request to accept the next inbound connection on a listener.
#[derive(Debug, Clone, PartialEq, Eq, Archive, Serialize, Deserialize)]
#[rkyv(bytecheck())]
pub struct NetAccept {
    /// Handle of the listener to accept on.
    pub handle: GuestResourceId,
}

/// Reply containing guest-visible handles for an accepted connection.
#[derive(Debug, Clone, PartialEq, Eq, Archive, Serialize, Deserialize)]
#[rkyv(bytecheck())]
pub struct NetAcceptReply {
    /// Reader handle registered in the instance registry.
    pub reader: GuestResourceId,
    /// Writer handle registered in the instance registry.
    pub writer: GuestResourceId,
    /// Address of remote caller (used for debugging).
    pub remote_addr: String,
}

/// Arguments for connecting to a remote endpoint.
#[derive(Debug, Clone, PartialEq, Archive, Serialize, Deserialize)]
#[rkyv(bytecheck())]
pub struct NetConnect {
    /// Protocol to use for the connection.
    pub protocol: NetProtocol,
    /// Authority or hostname of the remote peer.
    pub domain: String,
    /// Port number of the remote peer.
    pub port: u16,
    /// Optional TLS configuration handle for QUIC/HTTPS connections.
    pub tls: Option<GuestResourceId>,
}

/// Reply containing guest-visible handles for a connected session.
#[derive(Debug, Clone, PartialEq, Eq, Archive, Serialize, Deserialize)]
#[rkyv(bytecheck())]
pub struct NetConnectReply {
    /// Reader handle registered in the instance registry.
    pub reader: GuestResourceId,
    /// Writer handle registered in the instance registry.
    pub writer: GuestResourceId,
    /// Address of remote connection (used for debugging).
    pub remote_addr: String,
}
