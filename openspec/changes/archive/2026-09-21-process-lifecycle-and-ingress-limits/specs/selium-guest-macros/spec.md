## ADDED Requirements

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
