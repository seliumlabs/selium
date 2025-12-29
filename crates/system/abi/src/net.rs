use rkyv::{Archive, Deserialize, Serialize};

use crate::GuestResourceId;

/// Arguments for creating a network listener.
#[derive(Debug, Clone, PartialEq, Archive, Serialize, Deserialize)]
#[rkyv(bytecheck())]
pub struct NetCreateListener {
    /// Authority or hostname to bind to.
    pub domain: String,
    /// Port number to bind to.
    pub port: u16,
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

/// Arguments for connecting to a remote QUIC endpoint.
#[derive(Debug, Clone, PartialEq, Archive, Serialize, Deserialize)]
#[rkyv(bytecheck())]
pub struct NetConnect {
    /// Authority or hostname of the remote peer.
    pub domain: String,
    /// Port number of the remote peer.
    pub port: u16,
}

/// Reply containing guest-visible handles for a connected QUIC session.
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
