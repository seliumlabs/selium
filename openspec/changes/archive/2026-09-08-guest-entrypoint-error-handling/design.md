## Context

See proposal.md — Why. Entrypoints run on `wasm32-unknown-unknown` (no
WASI): the only input is the tagged `WasmValue` argument list the runtime
prepends/decodes — the discovery handle occupies slot 0. The `#[entrypoint]`
macro already classifies parameters by their syntactic type (leading
`Context`, integers, `(u64, u64)` pointers) and generates the `extern "C"`
glue; `run_entrypoint_with_result<F, T>` matches on the user function's
output and logs `error!`/returns 1 on `Err`. The guest SDK, wire, runtime,
and abi crates already derive `thiserror::Error`; only the guest-level
custom error types still hand-rolled `Error + Display`.

## Goals / Non-Goals

**Goals:**

- `Context::from_raw` failures degrade gracefully (`Err` → logged exit 1)
  for `Result`-returning entrypoints, instead of panicking.
- Every fallible entrypoint returns `anyhow::Result<()>`; startup steps use
  `?`/`with_context`/`bail!`.
- All custom error types use `#[derive(thiserror::Error)]`; no manual
  `impl std::error::Error` remains.
- Handle-forwarding guests (bridge) read the discovery handle from the
  context they already hold.

**Non-Goals:**

- Changing the ABI or exit codes (behavior-identical conversions).
- Making infallible `()` entrypoints fallible.
- `anyhow` in `selium-guest` itself (SDK errors stay typed
  `GuestError`; guests compose with `anyhow`).

## Decisions

### 1. Return-kind drives `from_raw` error handling

The macro expands `Context::from_raw(handle).await` with `?` when the
entrypoint returns `Result` and with `.expect(...)` when it returns `()`.
The `Result` path flows through the existing
`run_entrypoint_with_result` match (`Err(e)` → `error!` + exit 1);
`GuestError: std::error::Error` converts into any released error type
(`anyhow::Error` included) via the blanket `?` conversion.

- *Alternative:* always `?` — impossible for `()` entrypoints (no error
  channel); always `.expect` — forfeits graceful failure where an error
  channel exists. The per-return-kind branch keeps both behaviors.

### 2. `Context` owns the raw handle

`Context { handle: u64, ... }` plus `Context::raw_handle() -> u64`.
Bridge-server spawns bridge-channel children and must pass the discovery
handle as their slot-0 argument; previously it would have reconstructed
`Context` or threaded the raw value as separate state.

- *Alternative:* keep a copy of the raw value in bridge's entrypoint
  state — redundant with what `Context::from_raw` already consumed, and
  fragile if construction ever changes.

### 3. `anyhow::Result<()>` as the fallible entrypoint contract

Entrypoints build a named context per startup step
(`bag_with_context(|| "serve failed")`), `bail!` on argument/state
validation, and rely on the blanket `From<E: std::error::Error>` to fold
typed errors in. The macro's final `error!("{e}")` prints the whole
context chain, preserving pre-refactor failure wording.

- *Alternative:* a workspace `GuestEntryError` enum — adds a conversion
  layer and a crate to maintain for no ABI benefit; guests already carry
  heterogenous error types.

### 4. `thiserror::Error` derive for custom error types

Eight sites (`TlsError` ×2, `ForwardError`, `RpcError`, `HttpServeError`,
`InitError`, `WireError`, one macro-test error) convert to
`#[derive(Debug, Error)]` with per-variant `#[error("...")]`, matching
the existing style in `crates/{wire,guest,runtime,abi}`. Display strings
are byte-identical; `thiserror` derives `source()` for wrapped-error
fields (none of the plain-string variants carry a source). The two
connector guests and proto-dns add `thiserror.workspace = true`
(guest/wire already had it); the proc-macro crate's integration test adds
it as a dev-dependency.

- *Alternative:* keep manual impls — divergent style, and `source()`
  would remain unwired for future wrapped variants.

### 5. wasm32 compatibility

`anyhow` (`features = ["std"]`) and `thiserror` are compile-time:
`wasm32-unknown-unknown` has std, and every affected guest builds green
for that target (verified).

### 6. Parked entrypoints report success, not panic

Converting long-running service guests (discovery, bridge) and
event-waiting guests (net-demo's `accept().await`) to `Result<()>` routed
them through `run_entrypoint_with_result`, whose
`take_result().expect("entrypoint task must have completed")` panicked as
soon as the entrypoint parked — the normal state for a service — and
surfaced as a `Wasm("Trap: Unreachable")` bootstrap failure. The export
must return an exit code immediately, so a parked task reports `Ok(())`
(no error observed) and stays on the reactor, driven by later
`__selium_guest_poll` calls; the `Err` is logged inside the task whenever
it completes, because a late error can no longer reach the
already-returned exit code (the log is the only surfacing channel). The
`Display` bound previously enforced by the generated
`__selium_guest_assert_error` helper moves onto the function's `E:
core::fmt::Display` bound, and the macro wrapper only maps to the exit
code.

- *Alternative:* revert service guests to `()` entrypoints — forfeits the
  graceful `?`/`with_context` error handling this change introduces for
  exactly the guests with the most fallible startup.
- *Alternative:* keep the panic and require services to spawn their loop
  and return — an intrusive restructuring of every service guest for no
  behavioral gain.

## Risks / Trade-offs

- [Wrapped error variants now expose `source()`] → strictly additive;
  Display text unchanged; no test asserts a `None` source.
- [`Context::from_raw` panic path removed for `Result` entrypoints]
  → exit code stays 1, log emission unchanged; only the mechanism
  (panic vs `Err`) differs.
- [fmt normalizes untouched code (e.g. a guest `net/quic.rs` test
  assertion)] → benign style-only churn inside an already-touched crate;
  `cargo fmt --all` is a repo gate.

## Migration Plan

No runtime/ABI migration: duplicates of the old behavior do not coexist.
Docs (`SystemGuestDescriptor::set_discovery_handle`, OpenSpec wording)
updated in the same change so no stale "manual `Context::from_raw`"
guidance survives.

## Open Questions

None.