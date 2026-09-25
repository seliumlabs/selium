## MODIFIED Requirements

### Requirement: Context-Aware Entrypoint Parameter

`selium-guest-macros` SHALL parse the entrypoint function signature and
generate the argument-decoding wrapper for it. The accepted parameter list
is: an optional leading `Context` parameter, followed by zero or more
integer parameters (`u8`, `u16`, `u32`, `u64`, `usize`, `i8`, `i16`,
`i32`, `i64`, `isize`) and zero or more pointer parameters declared as
`(u64, u64)` (address, length). Each integer parameter consumes one
runtime argument slot; each pointer parameter consumes two slots (address
then length), in declaration order. Any other parameter type SHALL
produce a compile error. For `Context`-leading entrypoints the wrapper
SHALL construct the context from the first (discovery-handle) slot via
`Context::from_raw`. When the entrypoint returns `Result<(), E>`, a failed
construction SHALL propagate as the entrypoint's error value (the
generated wrapper logs the error and returns 1); when the entrypoint
returns `()`, a failed construction SHALL abort the guest.

#### Scenario: Macro injects Context into entrypoint

- **WHEN** a guest defines `#[entrypoint] async fn main(ctx: Context)`
- **THEN** the macro SHALL generate a wrapper that calls
  `Context::from_raw(...)` and passes the result to `main`

#### Scenario: Context bootstrap failure propagates for Result entrypoints

- **WHEN** a guest defines `#[entrypoint] async fn main(ctx: Context) -> Result<(), anyhow::Error>`
- **AND** `Context::from_raw` fails on the bootstrap handle
- **THEN** the generated wrapper SHALL return the construction error via `?`
- **AND** the entrypoint SHALL exit with code 1 after logging the error,
  rather than panicking

#### Scenario: Context bootstrap failure aborts unit entrypoints

- **WHEN** a guest defines `#[entrypoint] async fn main(ctx: Context)`
- **AND** `Context::from_raw` fails on the bootstrap handle
- **THEN** the generated wrapper SHALL abort the guest (the `()` return
  type provides no error channel)

#### Scenario: Macro forwards raw u64 argument into entrypoint

- **WHEN** a guest defines `#[entrypoint] async fn main(id: u64)`
- **THEN** the macro SHALL generate a wrapper that forwards the runtime-provided `i64` argument directly as the `u64` parameter

#### Scenario: Macro forwards multiple integer arguments

- **WHEN** a guest defines `#[entrypoint] async fn main(app_id: u32, generation: u64)`
- **THEN** the macro SHALL forward two runtime-provided `i64` values, narrowing `app_id` to `u32` and forwarding `generation` as `u64`

#### Scenario: Macro forwards a pointer argument

- **WHEN** a guest defines `#[entrypoint] async fn main(resolver: (u64, u64))`
- **THEN** the macro SHALL treat `resolver` as a pointer argument consuming two runtime-provided `i64` slots and pass `(address, length)` to `main`

#### Scenario: Context combines with trailing arguments

- **WHEN** a guest defines `#[entrypoint] async fn main(ctx: Context, resolver: (u64, u64))`
- **THEN** the macro SHALL construct `Context` from the discovery handle (first slot) and pass the pointer argument from the following two slots

#### Scenario: Unsupported parameter type rejected

- **WHEN** a guest defines `#[entrypoint] async fn main(name: String)`
- **THEN** the macro SHALL emit a compile error describing the accepted parameter types

### Requirement: Result Entrypoint Error Type Validation

`selium-guest-macros` SHALL produce a clear compile error when an
entrypoint returns `Result<(), E>` where `E` does not implement
`std::error::Error`. Error types derived with `thiserror::Error` satisfy
this bound (the derive implements `std::error::Error`), and are the
recommended form for guest custom error types.

#### Scenario: Non-Error error type rejected

- **WHEN** a guest defines `#[entrypoint] async fn main() -> Result<(), NotAnError>`
- **AND** `NotAnError` does not implement `std::error::Error`
- **THEN** the macro SHALL emit a compile error indicating that the error type must implement `std::error::Error`

#### Scenario: Error-typed error type accepted

- **WHEN** a guest defines `#[entrypoint] async fn main() -> Result<(), anyhow::Error>`
- **AND** `anyhow::Error` implements `std::error::Error`
- **THEN** the macro SHALL accept the return type

#### Scenario: Thiserror-derived error type accepted

- **WHEN** a guest defines `#[derive(Debug, thiserror::Error)] #[error("{0}")] struct ConfigError(String);` and `#[entrypoint] async fn main() -> Result<(), ConfigError>`
- **THEN** the macro SHALL accept the return type, because the derive implements `std::error::Error`