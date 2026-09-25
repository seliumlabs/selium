use selium_abi::ProcessId;
use thiserror::Error;
use wasmtiny::runtime::WasmError;

/// Result type used by the Selium kernel.
pub type Result<T> = std::result::Result<T, Error>;

/// Error returned by kernel operations.
#[derive(Debug, Error)]
pub enum Error {
    #[error("resource not found: {0}")]
    /// Requested resource was not found.
    NotFound(String),
    #[error("signal wait timed out")]
    /// Operation timed out.
    Timeout,
    #[error("request exchange already has a response")]
    /// Request exchange was already completed.
    AlreadyCompleted,
    #[error("process already stopped: {0}")]
    /// Process is already stopped.
    ProcessStopped(ProcessId),
    #[error("quota exceeded for tenant {tenant} on {class:?}")]
    /// Allocation would exceed the tenant's authored quota ceiling.
    QuotaExceeded {
        /// Tenant whose quota ceiling was exceeded.
        tenant: String,
        /// Resource class (dimension) whose ceiling was exceeded.
        class: selium_abi::ResourceClass,
    },
    #[error("wasmtiny runtime error: {0}")]
    /// Underlying Wasmtiny operation failed.
    Wasm(String),
}

pub(crate) fn map_wasm_error(error: WasmError) -> Error {
    Error::Wasm(error.to_string())
}
