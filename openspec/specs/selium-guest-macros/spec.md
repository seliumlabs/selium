## Purpose

Provide procedural macros for Selium guest entrypoints and generated guest interface metadata.

## Requirements

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

### Requirement: Result Entrypoint Error Type Validation
`selium-guest-macros` SHALL produce a clear compile error when an entrypoint returns `Result<(), E>` where `E` does not implement `std::error::Error`. Error types derived with `thiserror::Error` satisfy this bound (the derive implements `std::error::Error`), and are the recommended form for guest custom error types.

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

### Requirement: Backward Compatible Entrypoint Return Types
`selium-guest-macros` SHALL continue to accept and generate code for entrypoints returning `()` without requiring any changes to existing guest code.

#### Scenario: Existing void entrypoint unchanged
- **WHEN** a guest defines `#[entrypoint] async fn main()`
- **THEN** the generated glue SHALL be identical in behavior to the current implementation
- **AND** the export SHALL have no return value

### Requirement: Generated Pattern Metadata
`selium-guest-macros` SHALL generate metadata for guest-declared messaging interfaces so that discovery and binding layers can reason about them consistently.

#### Scenario: Messaging interface metadata emitted
- **WHEN** a guest declares a messaging interface using the macro layer
- **THEN** the macro SHALL emit metadata describing the interface in a form consumable by the guest SDK and runtime tooling

### Requirement: Bootstrap-Aware Macro Integration
`selium-guest-macros` SHALL interoperate with the guest SDK so generated entrypoints and messaging metadata can participate in runtime bootstrap and tracing setup.

#### Scenario: Runtime bootstraps macro-based guest
- **WHEN** the runtime starts a guest built with the macro layer
- **THEN** the generated glue SHALL remain compatible with runtime bootstrap expectations and guest tracing setup

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
- **THEN** the macro SHALL generate a wrapper that calls `Context::from_raw(...)` and passes the result to `main`

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

### Requirement: Wire parameter on schema macro

The `#[schema]` macro SHALL accept an optional `wire = WireTypeName` parameter. When present, the macro SHALL generate a wire struct and bridge `FlatMsg` impl instead of generating `FlatMsg`/`FieldEncoder`/`write_flatbuffer`/`from_flatbuffer` directly on the annotated type.

#### Scenario: Wire parameter triggers wire type generation

- **WHEN** a struct with generic parameters is annotated with `#[schema(path = "...", ty = "...", binding = "...", wire = MyWire)]`
- **THEN** the macro SHALL generate a struct named `MyWire` with the `#[schema]` annotation applied
- **AND** the macro SHALL generate a `FlatMsg` impl on the original struct that bridges through `MyWire`

#### Scenario: Wire parameter is optional

- **WHEN** a struct without generic parameters is annotated with `#[schema(path = "...", ty = "...", binding = "...")]` (no `wire`)
- **THEN** the macro SHALL behave exactly as before (direct `FlatMsg`/`FieldEncoder` generation on the annotated type)

### Requirement: Generic parameter auto-detection for byte fields

The macro SHALL automatically detect which struct fields reference generic type parameters. Fields whose type references a generic parameter SHALL be mapped to `Vec<u8>` in the generated wire struct with the `_bytes` suffix appended to the field name.

#### Scenario: Direct generic field detection

- **WHEN** `struct Msg<K> { pub key: K }` is processed
- **THEN** the macro SHALL identify `key` as a byte-serialized field because `K` is a generic parameter
- **AND** the generated wire field SHALL be `pub key_bytes: Vec<u8>`

#### Scenario: Option-wrapped generic field detection

- **WHEN** `struct Msg<V> { pub value: Option<V> }` is processed
- **THEN** the macro SHALL identify `value` as a byte-serialized field because `V` is a generic parameter inside `Option`
- **AND** the generated wire field SHALL be `pub value_bytes: Vec<u8>`

#### Scenario: Concrete type field is not detected as generic

- **WHEN** `struct Msg<K> { pub id: u64 }` is processed and `u64` is not a generic parameter
- **THEN** the macro SHALL NOT treat `id` as byte-serialized
- **AND** the generated wire field SHALL be `pub id: u64` (pass-through)

### Requirement: Bridge encode impl for generic types

The macro SHALL generate `FlatMsg::encode` on the domain type that: for each byte-serialized field, calls `FlatMsg::encode` on the field value to produce `Vec<u8>`; for each `Option<generic>` field, encodes `Some` values to bytes and `None` to empty `Vec`; for each `Option<scalar>` field, converts `None` to the zero sentinel; constructs the wire struct with all converted fields; and delegates to `FlatMsg::encode(&wire)`.

#### Scenario: Encode serializes generics to bytes

- **WHEN** `encode` is called on `Msg { key: "hello".to_string() }` where `key: K` and `K = String`
- **THEN** the generated code SHALL call `FlatMsg::encode(&value.key)` to produce the bytes for `key_bytes`

#### Scenario: Encode handles None option generics

- **WHEN** `encode` is called with `value: None` where `value: Option<V>`
- **THEN** the generated code SHALL set `value_bytes` to an empty `Vec`

#### Scenario: Encode handles None option scalars

- **WHEN** `encode` is called with `expected_version: None` where `expected_version: Option<u64>`
- **THEN** the generated code SHALL set `expected_version` to `0` in the wire struct

### Requirement: Bridge decode impl for generic types

The macro SHALL generate `FlatMsg::decode` on the domain type that: decodes the wire struct via `FlatMsg::decode(bytes)`; for each byte-serialized field, calls `FlatMsg::decode(&wire.field_bytes)` to reconstruct the typed value; for each `Option<generic>` field, returns `None` if bytes are empty and `Some(FlatMsg::decode(...))` otherwise; for each `Option<scalar>` field, converts the zero sentinel to `None`.

#### Scenario: Decode reconstructs generics from bytes

- **WHEN** `decode` is called with bytes containing an encoded `Msg<String>`
- **THEN** the generated code SHALL decode the wire struct, then decode `key_bytes` via `FlatMsg::decode::<String>(&wire.key_bytes)`

#### Scenario: Decode handles empty bytes as None

- **WHEN** `decode` encounters a wire struct with empty `value_bytes`
- **THEN** the generated code SHALL produce `value: None` in the domain struct

#### Scenario: Decode handles zero sentinel as None

- **WHEN** `decode` encounters a wire struct with `expected_version: 0`
- **THEN** the generated code SHALL produce `expected_version: None` in the domain struct

### Requirement: Poll Export Completion Signal

`selium-guest-macros` SHALL generate, for a wasm build, the unmangled `__selium_guest_poll` export that delegates to `selium_guest::poll_safely()` and returns its reactor completion code as an `i32` — `0` while the poll-owner entrypoint future is still running (parked) and `1` once it has completed — so the host records `ProcessExited` and tears the process down as a normal exit. The export SHALL be `#[cfg(target_family = "wasm")]`-gated and SHALL NOT be emitted in native builds.

#### Scenario: Poll export reports completion code

- **WHEN** the host polls a guest whose poll-owner entrypoint future has completed
- **THEN** the generated `__selium_guest_poll` export SHALL return `1`, signalling process completion

#### Scenario: Poll export reports running while parked

- **WHEN** the host polls a guest whose poll-owner entrypoint future is parked (a long-running service)
- **THEN** the generated `__selium_guest_poll` export SHALL return `0`, leaving the process resident

#### Scenario: No poll export in native builds

- **WHEN** a guest crate is compiled for a native target
- **THEN** the generated glue SHALL NOT emit the `__selium_guest_poll` export (avoiding symbol collisions between entrypoint crates linked into one native binary)
