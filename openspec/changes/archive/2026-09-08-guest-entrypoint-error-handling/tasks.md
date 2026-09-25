# Tasks: Guest Entrypoint Error Handling

## 1. Macro and guest SDK

- [x] 1.1 Rework the macro's `Context` prelude to emit `Context::from_raw(handle).await?` for `Result`-returning entrypoints and keep `.expect(...)` for `()` entrypoints; verify host + `wasm32-unknown-unknown` builds of all guests.
- [x] 1.2 Add `handle: u64` to `Context` (set in `from_raw`) with `Context::raw_handle()`; verify guest SDK unit tests pass and clippy is clean.
- [x] 1.3 Convert the macro integration test error to `#[derive(thiserror::Error)]` (dev-dependency on `selium-guest-macros`); verify `cargo test -p selium-guest-macros` passes.
- [x] 1.4 Fix `run_entrypoint_with_result` for parked entrypoints: converting the long-running service guests (discovery, bridge) and event-waiting guests (net-demo's `accept`) to `Result<()>` routed them through `take_result().expect(...)`, which panicked the moment the entrypoint parked — surfacing as `Wasm("Trap: Unreachable")` at bootstrap (discovery, quic_spine, net_wake, fastpath_wake tests). The function now returns `Ok(())` when the task parks (exit code 0, task stays on the reactor for later polls) and logs the `Err` inside the task whenever it completes; the macro wrapper maps to the exit code only. Verify SDK unit tests (parked + failing entrypoints), the regenerated trybuild stderr, and all six ignored wasm integration tests pass.

## 2. Guest entrypoint conversions to anyhow::Result

- [x] 2.1 Convert bridge-server and bridge-channel entrypoints to `anyhow::Result<()>` (`?`, `with_context`, `bail!`), with bridge-server forwarding `ctx.raw_handle()` to spawned bridge-channels (Context-first argument order); verify bridge/bridge-channel host checks, clippy, and tests pass.
- [x] 2.2 Convert connector-dns, connector-quic, connector-http entrypoints (resolver/lookup validation via `bail!`, startup steps via `?`; per-connection logging preserved); verify host checks, clippy, and wasm32 builds pass.
- [x] 2.3 Convert discovery, discovery-probe, dns-demo, quic-demo, net-demo, spine-demo entrypoints (anyhow already present on discovery-probe; add `anyhow = { workspace = true, features = ["std"] }` to the 10 fallible guests); verify all guest wasm32 builds and guest/runtime test suites pass.
- [x] 2.4 Leave the four infallible entrypoints (external-api, supervisor, scheduler, cluster) as `()`; verify workspace `cargo check --all-targets` stays clean.

## 3. Error type standardization (thiserror)

- [x] 3.1 Convert connector-quic/connector-http `TlsError` and connector-http `ForwardError` to `#[derive(thiserror::Error)]`, add `thiserror.workspace = true` to both connectors; verify host checks, clippy, and wasm32 builds.
- [x] 3.2 Convert `RpcError` (wire), `HttpServeError` + `InitError` (guest SDK), and `WireError` (proto-dns, new `thiserror` dep) to derives; remove now-unused `fmt` imports; verify wire/guest/proto-dns checks, clippy, and tests.
- [x] 3.3 Confirm zero `impl std::error::Error` remain in `guests/` and `crates/` (excluding intentional UI test fixtures); verify `cargo fmt --all` clean.

## 4. Docs, specs, and gates

- [x] 4.1 Fix the stale `SystemGuestDescriptor::set_discovery_handle` doc (macro constructs `Context`; raw handle goes directly to `u64`-leading entrypoints); verify doc-target check compiles.
- [x] 4.2 Author OpenSpec delta: `selium-guest-macros` (Context failure per return kind + thiserror-accepted scenario), `selium-runtime` (discovery-handle wording: macro constructs `Context`), `selium-guest` (raw handle accessor requirement); verify `openspec validate` and `openspec status` report all artifacts ready.
- [x] 4.3 Gates: `cargo fmt --all`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test` for guest SDK, guest-macros, wire, proto-dns, runtime; `cargo build --target wasm32-unknown-unknown` for all guests.