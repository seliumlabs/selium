//! The client's single error surface.
//!
//! Connection, TLS, transport, framing, RPC, and termination failures all
//! collapse into [`Error`]. Underlying `selium-wire` errors pass through the
//! [`Error::Wire`] variant; where the client can enrich (termination codes,
//! RPC failures) it maps to dedicated variants instead.

use selium_wire::{RpcError, TERMINATE_ATTACH_FAILED, TERMINATE_BAD_HANDSHAKE};
use thiserror::Error;

/// Result type for `selium-client` operations.
pub type Result<T> = std::result::Result<T, Error>;

/// Single flat error type covering every client failure mode.
#[derive(Debug, Error)]
pub enum Error {
    #[error("failed to connect: {0}")]
    Connect(#[from] quinn::ConnectError),
    #[error("connection error: {0}")]
    Connection(#[from] quinn::ConnectionError),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    /// A transport, framing, or channel error passed through from
    /// `selium-wire`.
    #[error("wire error: {0}")]
    Wire(selium_wire::Error),
    #[error("serialization error: {0}")]
    Serialization(String),
    /// The remote peer terminated a stream with an application error.
    #[error("remote stream error: {0}")]
    Remote(String),
    #[error("TLS configuration error: {0}")]
    Tls(String),
    /// The bridge rejected the handshake frame sent at channel open.
    #[error("bridge rejected the handshake")]
    BadHandshake,
    /// The bridge could not resolve or attach the requested channel.
    #[error("bridge could not attach the requested channel")]
    AttachFailed,
    /// The bridge terminated the channel with an unrecognised code.
    #[error("channel terminated by the bridge (code {0})")]
    Terminated(u32),
}

impl From<selium_wire::Error> for Error {
    fn from(error: selium_wire::Error) -> Self {
        // Enrich where the client has its own variant; pass the rest through.
        match error {
            selium_wire::Error::SerializationFailed(message) => Error::Serialization(message),
            other => Error::Wire(other),
        }
    }
}

impl From<RpcError> for Error {
    fn from(error: RpcError) -> Self {
        match error {
            RpcError::ConnectionClosed => Error::Wire(selium_wire::Error::Terminated),
            RpcError::Serialization(message) => Error::Serialization(message),
            RpcError::Remote(message) => Error::Remote(message),
            RpcError::InvalidRegion => Error::Wire(selium_wire::Error::InvalidRegion),
            RpcError::LayoutMismatch => Error::Wire(selium_wire::Error::LayoutMismatch),
            RpcError::BufferFull => Error::Wire(selium_wire::Error::BufferFull),
            RpcError::BufferEmpty => Error::Wire(selium_wire::Error::BufferEmpty),
        }
    }
}

/// Maps a bridge termination code to its typed error variant.
pub(crate) fn termination_error(code: u32) -> Error {
    match code {
        TERMINATE_BAD_HANDSHAKE => Error::BadHandshake,
        TERMINATE_ATTACH_FAILED => Error::AttachFailed,
        other => Error::Terminated(other),
    }
}
