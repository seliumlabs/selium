use std::io;

use selium_abi::{AbiError, AbiErrorCode};
use thiserror::Error;

/// Result type used by the Selium guest SDK.
pub type Result<T> = std::result::Result<T, GuestError>;

/// Error returned by guest SDK operations.
#[derive(Debug, Error)]
pub enum GuestError {
    #[error("host error: {0}")]
    /// Host-side operation failed.
    Host(String),
    #[error("codec error: {0}")]
    /// Encoding or decoding failed.
    Codec(#[from] selium_abi::RkyvError),
    #[error("permission denied: {0}")]
    /// The host denied access to a capability, with the host-supplied detail.
    PermissionDenied(String),
    #[error("unexpected hostcall output")]
    /// The host returned a response variant that did not match the request.
    UnexpectedHostcallOutput,
    #[error("builder is sealed")]
    /// Builder does not accept further modifications.
    BuilderSealed,
    #[error("capacity exceeded")]
    /// Capacity limit exceeded.
    CapacityExceeded,
    #[error("I/O error: {0}")]
    Io(io::Error),
}

pub(crate) fn abi_error_to_guest_error(error: AbiError) -> GuestError {
    if error.code == AbiErrorCode::PermissionDenied {
        GuestError::PermissionDenied(error.message)
    } else {
        GuestError::Host(format!("{:?}: {}", error.code, error.message))
    }
}
