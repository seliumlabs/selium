## Why

The control plane already serves a typed, capability-gated control surface to externally authenticated clients, and `selium-client` already collapses QUIC, TLS/mTLS, the bridge handshake, and wire framing behind typed handles. What is missing is a user-facing way to drive that surface: the `sel` CLI was explicitly deferred by `implement-control-plane` ("No CLI crate in this change; the `sel` CLI is a follow-up consuming the served surface"). Operators need a thin, typed way to deploy, scale, stop, inspect, resolve, and upload through the `control.<tenant>` route without hand-rolling a `selium-client` program.

## What Changes

- New native `selium-cli` crate at `crates/cli` (package `selium-cli`, binary `sel`), added as a workspace member. It is a thin wrapper over `selium-client` — no new wire format, no guest changes, no runtime changes.
- One clap subcommand per control-plane verb, mapping one-to-one to `ControlRequest` variants served at `sel://<tenant>/control`:
  - `deploy <workload> --replicas N --module M` → `Deploy`
  - `scale <workload> --replicas N` → `Scale`
  - `stop <workload>` → `Stop`
  - `status <workload>` → `Status`
  - `resolve <uri>` → `Resolve`
  - `upload --manifest <name> --file <path>` → `Upload` (module bytes read from a file)
- Ephemeral lifecycle: each invocation establishes one QUIC connection, opens one RPC channel to the control route, performs a single request/reply, prints the result, and exits. No persistent connection or shell mode.
- Target naming derives the connection from the tenant: `--tenant <tenant>` produces the SNI/server name `bridge.<tenant>` (the bridge route the connector delivers to) and the control route `sel://<tenant>/control`; `--connector <addr>` is the QUIC endpoint.
- Mutual TLS is presented but not provisioned: `--ca` supplies the server root certificate to trust, `--client-cert`/`--client-key` supply the client identity passed to `ConnectOptions`. Identity provisioning remains out of scope.
- Typed failure mapping: `Accepted { delegated.applied == false }` surfaces the scheduler's deferred status as a failed deploy (printed context, non-zero exit), and `ControlResponse::Error` surfaces its `step`/`context` with a non-zero exit. `status` for an unknown workload prints a "not found" result and exits non-zero.

## Capabilities

### New Capabilities

- `selium-cli`: the `sel` command-line client — the subcommand surface over the control plane's typed verbs, connection setup driven by tenant and connector address, bridge hostname derivation, and the typed response → output/exit-code mapping.

### Modified Capabilities

<!-- No existing capability's requirements change: the CLI consumes the
     control-plane, addressing, and selium-client surfaces as already
     specified, without altering their behaviour. -->

## Impact

- New crate `crates/cli` and new `[[bin]] name = "sel"` target; `Cargo.toml` workspace membership. Depends on `selium-client` (which re-exports the `selium-service` control message types and `FlatMsg`), `clap` (derive), and `tokio`.
- No changes to `guests/control-plane`, the bridge, `selium-wire`, or the runtime: the CLI is a peer of the existing `control_plane_bridge` integration test's client half, minus the test harness.
- Tests stay at the command-mapping level per the agreed scope: argv → `ControlRequest`, and `ControlResponse` → output/exit code, exercised against a native seam. A full external end-to-end test against the WASM guests (and against the interim mTLS fixtures under `guests/connector-quic/tests/fixtures/`) is deferred to a follow-up.
- Deliberately out of scope for this change: `sel logs` focus-mode log following (a separate change with its own platform prerequisites — a discovery route for a process's log ring and a counting writer on the log channel), JSON output, persistent/shell mode, workload naming, and mTLS identity provisioning.
