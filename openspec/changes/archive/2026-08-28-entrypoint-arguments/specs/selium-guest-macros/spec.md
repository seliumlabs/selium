## MODIFIED Requirements

### Requirement: Context-Aware Entrypoint Parameter

`selium-guest-macros` SHALL parse the entrypoint function signature and
generate the argument-decoding wrapper for it. The accepted parameter list
is: an optional leading `Context` parameter, followed by zero or more
integer parameters (`u8`, `u16`, `u32`, `u64`, `i8`, `i16`, `i32`, `i64`)
and zero or more pointer parameters declared as `(u64, u64)` (address,
length). Each integer parameter consumes one runtime argument slot; each
pointer parameter consumes two slots (address then length), in declaration
order. Any other parameter type SHALL produce a compile error.

#### Scenario: Macro injects Context into entrypoint

- **WHEN** a guest defines `#[entrypoint] async fn main(ctx: Context)`
- **THEN** the macro SHALL generate a wrapper that calls `Context::from_raw(...)` and passes the result to `main`

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
