## MODIFIED Requirements

### Requirement: Guest Entrypoint Macro
`selium-guest-macros` SHALL provide a macro that generates the ABI glue needed to expose a guest entrypoint through `selium-abi` and `selium-guest`.

#### Scenario: Macro-generated entrypoint glue
- **WHEN** a guest author annotates a supported async entrypoint with the provided macro
- **THEN** the macro SHALL generate the required ABI-compatible entry glue

#### Scenario: Macro-generated entrypoint glue returns i32 for Result
- **WHEN** a guest author annotates an entrypoint returning `Result<()>` with the provided macro
- **THEN** the macro SHALL generate an `extern "C" fn` that returns `i32`
- **AND** the generated wrapper SHALL return `0` when the user function returns `Ok(())`
- **AND** the generated wrapper SHALL log the error via `selium_guest::error!` and return `1` when the user function returns `Err(e)`

### Requirement: Context-Aware Entrypoint Parameter
`selium-guest-macros` SHALL detect when the entrypoint function takes a `Context` parameter or a `u64` raw argument and generate the appropriate wrapper that constructs and injects the parameter before calling the user function.

#### Scenario: Macro injects Context into entrypoint
- **WHEN** a guest defines `#[entrypoint] async fn main(ctx: Context)`
- **THEN** the macro SHALL generate a wrapper that calls `Context::from_raw(...)` and passes the result to `main`

#### Scenario: Macro forwards raw u64 argument into entrypoint
- **WHEN** a guest defines `#[entrypoint] async fn main(id: u64)`
- **THEN** the macro SHALL generate a wrapper that forwards the runtime-provided `i64` argument directly as the `u64` parameter

## ADDED Requirements

### Requirement: Result Entrypoint Error Type Validation
`selium-guest-macros` SHALL produce a clear compile error when an entrypoint returns `Result<(), E>` where `E` does not implement `std::error::Error`.

#### Scenario: Non-Error error type rejected
- **WHEN** a guest defines `#[entrypoint] async fn main() -> Result<(), NotAnError>`
- **AND** `NotAnError` does not implement `std::error::Error`
- **THEN** the macro SHALL emit a compile error indicating that the error type must implement `std::error::Error`

#### Scenario: Error-typed error type accepted
- **WHEN** a guest defines `#[entrypoint] async fn main() -> Result<(), anyhow::Error>`
- **AND** `anyhow::Error` implements `std::error::Error`
- **THEN** the macro SHALL accept the return type

### Requirement: Backward Compatible Entrypoint Return Types
`selium-guest-macros` SHALL continue to accept and generate code for entrypoints returning `()` without requiring any changes to existing guest code.

#### Scenario: Existing void entrypoint unchanged
- **WHEN** a guest defines `#[entrypoint] async fn main()`
- **THEN** the generated glue SHALL be identical in behavior to the current implementation
- **AND** the export SHALL have no return value
