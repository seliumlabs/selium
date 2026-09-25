use selium_abi::{AbiError, AbiErrorCode, Capability, ProcessId};
use thiserror::Error;
use wasmtiny::WasmError;

/// Result type used by the Selium runtime.
pub type Result<T> = std::result::Result<T, Error>;

/// Error returned by runtime operations.
#[derive(Debug, Error)]
pub enum Error {
    #[error("system guest descriptor not found: {0}")]
    /// System guest descriptor was not found.
    DescriptorNotFound(String),
    #[error("unknown process authority: {0}")]
    /// Process authority was not registered.
    UnknownProcessAuthority(ProcessId),
    #[error("unknown dependency: {0}")]
    /// System guest dependency is not present in the configuration.
    UnknownDependency(String),
    #[error("dependency cycle or unresolved dependency detected")]
    /// System guest dependencies cannot be resolved.
    DependencyCycle,
    #[error("invalid grant for capability {0:?}")]
    /// Capability grant is invalid.
    InvalidGrant(Capability),
    #[error("grant for capability {0:?} contains unevaluatable selector: {1}")]
    /// Grant contains a selector the runtime cannot evaluate.
    UnevaluatableSelector(Capability, String),
    #[error("duplicate system guest descriptor: {0}")]
    /// Duplicate system guest name was supplied.
    DuplicateDescriptor(String),
    #[error("module id already registered with different bytes: {0}")]
    /// Module id was already registered with different bytes.
    ModuleConflict(String),
    #[error("module not registered: {0}")]
    /// Module id is not registered.
    UnknownModule(String),
    #[error("invalid entrypoint argument encoding")]
    /// Entrypoint argument bytes are invalid.
    InvalidEntrypointArgument,
    #[error("readiness condition not satisfied for guest `{0}`")]
    /// System guest did not satisfy its readiness condition.
    ReadinessUnsatisfied(String),
    #[error("guest entrypoint returned an error: {0}")]
    /// Guest entrypoint returned a non-zero exit code.
    EntrypointFailed(String),
    #[error("kernel error: {0}")]
    /// Kernel operation failed.
    Kernel(#[from] selium_kernel::Error),
    #[error("wasmtiny runtime error: {0}")]
    /// Underlying Wasmtiny operation failed.
    Wasm(String),
    #[error("host operation failed: {0}")]
    /// Host-side operation failed (e.g., discovery RPC).
    Host(String),
}

pub(crate) fn kernel_error(error: selium_kernel::Error) -> AbiError {
    let code = match error {
        selium_kernel::Error::NotFound(_) => AbiErrorCode::NotFound,
        selium_kernel::Error::Timeout => AbiErrorCode::Timeout,
        // A quota denial is not a permission problem: the caller holds the
        // capability, its tenant's authored ceiling is exhausted. The
        // message names the tenant and resource class (dimension).
        selium_kernel::Error::QuotaExceeded { .. } => AbiErrorCode::QuotaExceeded,
        selium_kernel::Error::AlreadyCompleted
        | selium_kernel::Error::ProcessStopped(_)
        | selium_kernel::Error::Wasm(_) => AbiErrorCode::Internal,
    };
    AbiError::new(code, error.to_string())
}

pub(crate) fn map_wasm_error(error: WasmError) -> Error {
    Error::Wasm(error.to_string())
}
