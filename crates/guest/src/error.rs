use std::io;

use selium_abi::{AbiError, AbiErrorCode};
use thiserror::Error;

/// Result type used by the Selium guest SDK.
pub type Result<T> = std::result::Result<T, GuestError>;

/// Error returned by guest SDK operations.
#[derive(Debug, Error)]
pub enum GuestError {
    #[error("host error: {0}")]
    /// Host-side operation failed without a structured ABI cause.
    Host(String),
    #[error("hostcall failed ({code:?}): {message}")]
    /// A hostcall failed with the host's stable ABI error code and message.
    Hostcall {
        /// Stable machine-readable ABI error code.
        code: AbiErrorCode,
        /// Human-readable host-supplied detail.
        message: String,
    },
    #[error("codec error: {0}")]
    /// Encoding or decoding failed.
    Codec(#[from] selium_abi::RkyvError),
    #[error("permission denied: {0}")]
    /// The host denied access to a capability, with the host-supplied detail.
    PermissionDenied(String),
    #[error("unexpected hostcall output")]
    /// The host returned a response variant that did not match the request.
    UnexpectedHostcallOutput,
    #[error("I/O error: {0}")]
    Io(io::Error),
}

impl GuestError {
    /// Returns the stable ABI error code carried by a structured hostcall
    /// failure, if this error came from one.
    pub fn abi_code(&self) -> Option<AbiErrorCode> {
        match self {
            Self::Hostcall { code, .. } => Some(code.clone()),
            _ => None,
        }
    }

    /// Adds operation context without discarding a structured ABI cause.
    pub fn context(self, context: impl std::fmt::Display) -> Self {
        match self {
            Self::Hostcall { code, message } => Self::Hostcall {
                code,
                message: format!("{context}: {message}"),
            },
            Self::Host(message) => Self::Host(format!("{context}: {message}")),
            other => other,
        }
    }
}

pub(crate) fn abi_error_to_guest_error(error: AbiError) -> GuestError {
    GuestError::Hostcall {
        code: error.code,
        message: error.message,
    }
}

impl From<AbiError> for GuestError {
    fn from(error: AbiError) -> Self {
        abi_error_to_guest_error(error)
    }
}

impl From<selium_memory::MemoryError> for GuestError {
    fn from(error: selium_memory::MemoryError) -> Self {
        match error {
            selium_memory::MemoryError::Abi(error) => error.into(),
            other => GuestError::Host(other.to_string()),
        }
    }
}

impl From<selium_wire::Error> for GuestError {
    fn from(error: selium_wire::Error) -> Self {
        match error {
            selium_wire::Error::Abi(error) => error.into(),
            other => GuestError::Host(other.to_string()),
        }
    }
}

impl From<selium_wire::RpcError> for GuestError {
    fn from(error: selium_wire::RpcError) -> Self {
        match error {
            selium_wire::RpcError::Abi(error) => error.into(),
            other => GuestError::Host(other.to_string()),
        }
    }
}

pub(crate) fn guest_error_to_wire_error(error: GuestError) -> selium_wire::Error {
    match error {
        GuestError::Hostcall { code, message } => {
            selium_wire::Error::Abi(AbiError { code, message })
        }
        other => selium_wire::Error::Guest(other.to_string()),
    }
}

pub(crate) fn guest_error_to_memory_error(error: GuestError) -> selium_memory::MemoryError {
    match error {
        GuestError::Hostcall { code, message } => {
            selium_memory::MemoryError::Abi(AbiError { code, message })
        }
        other => selium_memory::MemoryError::Other(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // scenario: selium-guest/guest-errors-preserve-stable-abi-causes/quota-failure-is-matchable
    #[test]
    fn abi_errors_preserve_code_and_message() {
        let cases = [
            AbiErrorCode::PermissionDenied,
            AbiErrorCode::QuotaExceeded,
            AbiErrorCode::MalformedPayload,
            AbiErrorCode::Timeout,
            AbiErrorCode::NotFound,
            AbiErrorCode::Internal,
        ];
        for code in cases {
            let error = abi_error_to_guest_error(AbiError::new(
                code.clone(),
                format!("detail for {code:?}"),
            ));
            assert_eq!(error.abi_code(), Some(code.clone()));
            match &error {
                GuestError::Hostcall {
                    code: actual,
                    message,
                } => {
                    assert_eq!(actual, &code);
                    assert_eq!(message, &format!("detail for {code:?}"));
                }
                other => panic!("expected a structured hostcall error, got {other:?}"),
            }
        }
    }
}
