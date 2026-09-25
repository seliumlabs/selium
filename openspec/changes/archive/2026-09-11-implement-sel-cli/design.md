## Context

See `proposal.md` - Why for motivation. The substrate facts that shape the approach:

- `selium-client` already provides the whole client half of the path: `connect(SocketAddr, ConnectOptions)` (QUIC, TLS 1.3, optional mTLS identity) and `Client::rpc::<Req, Rep>(uri)` (open a stream, send the typed bridge handshake, await the deterministic acceptance reply, return a `RpcClient`). It re-exports `selium_service`, where `ControlRequest`/`ControlResponse` live.
- The control plane serves `sel://<tenant>/control`; the QUIC connection's SNI must name the bridge route (`bridge.<tenant>`), which the connector resolves and delivers to the tenant's bridge-server. The channel handshake names the control route.
- `crates/runtime/tests/control_plane_bridge.rs` is the reference for the external client half: `connect` → `client.rpc::<ControlRequest, ControlResponse>("sel://acme/control")` → `rpc.request(...)`. The CLI is that, generalised and behind clap.
- The day-1 scheduler seam returns `Accepted { delegated: Deferred("scheduler service not yet online") }`; the control plane records desired state but does not apply placement yet.

## Goals / Non-Goals

**Goals:**

- A single native binary `sel` that turns argv into exactly one `ControlRequest`, executes it over one connection and one channel, renders one human-readable line (or error), and exits with a 0/non-zero code.
- Pure, unit-testable mapping seams: argv → `ControlRequest`, and (command, `ControlResponse`) → (output text, exit code), independent of any live connection.

**Non-Goals:**

- No persistent connection, shell/REPL, or request batching (ephemeral only, per the spec).
- No JSON or structured output.
- No `sel logs` (a separate change with platform prerequisites), workload naming, or mTLS identity provisioning.

## Decisions

### 1. Crate shape: `crates/cli`, binary-only, three modules

**Decision:** `crates/cli` with package `selium-cli` and `[[bin]] name = "sel"`. Three modules:

- `main.rs` — entrypoint: `#[tokio::main]`, top-level error → `eprintln!` + exit code.
- `cli.rs` — the clap surface (`Cli`, `Command`, flag/arg structs).
- `commands.rs` — pure mapping functions: build the `ControlRequest` from parsed args, and map the `ControlResponse` to a rendered line + exit code.

**Rationale:** Keeping the mapping functions free of `connect` and the tokio runtime makes the command-mapping tests trivial and mirrors the project's "test the seam natively" habit. A `lib` target is unnecessary.

**Alternative considered:** Everything inline in `main.rs`.
- Rejected: untestable mapping and a busy entrypoint.

### 2. Dependency set is minimal

**Decision:** `selium-client` (pulls `selium-service` + `FlatMsg`), `clap` (derive/help/std, already a workspace dep), `tokio` (macros, rt), `anyhow` for top-level plumbing. No `tracing`/`tracing-subscriber` — the CLI is native and reports through `eprintln!`.

**Rationale:** The CLI contributes no new wire or encoding surface; `selium-client` re-exports the control message types, so there is no direct `selium-service` or `selium-wire` dependency.

### 3. Target naming derives from tenant only

**Decision:** The CLI takes `--tenant` and `--connector`, and derives both names:

```
server_name  = "bridge.{tenant}"          (SNI + cert verification name)
control_route = "sel://{tenant}/control"  (channel handshake URI)
```

`--connector` is required with no default (the `127.0.0.1:4433` fixture is a test concern, not a product default). Trust and identity are `--ca` / `--client-cert` / `--client-key`, passed through to `ConnectOptions`.

**Rationale:** Matches the architecture: the bridge route is the only entry point, and the tenant is the one piece of information a user reliably knows. Always deriving, never asking for both names, avoids drift.

**Alternative considered:** `--server-name` / `--url` overrides.
- Rejected: unnecessary surface today; can be added later without changing the spec.

### 4. One request per invocation, one channel

**Decision:** Each verb handler calls `connect(addr, options)` once, then `client.rpc::<ControlRequest, ControlResponse>(control_route)` once, sends one request, renders the reply, and drops both handles (which tears the channel and connection down).

**Rationale:** The surface is plain request/reply (no watch/stream verb), so an ephemeral instance is the honest and simplest shape. Dropping the handles exercises the same bridge teardown the integration test relies on.

### 5. Failure mapping is centralised in one function

**Decision:** A single `render(&Command, ControlResponse) -> (String, ExitCode)` covers every response variant; the parsed command contextualises the success line because `ControlResponse::Accepted` does not identify which verb produced it:

- `Accepted { delegated, .. }` with `delegated.applied == false` → print `delegated.context`, treat as failure (non-zero), regardless of verb.
- `Accepted { .. }` with `delegated.applied == true` → print the verb's own outcome: `deploy` reports "accepted with N replicas and module M", `scale` reports "scaled to N replicas", `stop` reports "stopped"; any other verb receiving `Accepted` is a protocol mismatch and gets the generic accepted line.
- `Error { step, context }` → print `step: context`, non-zero.
- `Status { deployment: None }` and `Resolved { target: None }` → print "not found", non-zero.
- Success variants → print the outcome, zero.

Transport/clap/IO failures (unreadable `--file`, unreadable cert material, connect/handshake/open failures) all funnel through the top-level `anyhow` error path to a non-zero exit.

**Rationale:** One place to reason about "what makes a command fail" keeps the CLI honest about the day-1 `Deferred` status; when the scheduler service lands and delegation starts returning `applied: true`, this behaviour self-corrects with no CLI change.

**Alternative considered:** Only treat `ControlResponse::Error` as failure, and print `Deferred` as success.
- Rejected: a `deploy` that recorded intent but cannot place anything is a failure from the operator's point of view; hiding it would be a lie about the day-1 system.

### 6. Verb ↔ request mapping is one-to-one and lossless

**Decision:** `deploy` → `Deploy { workload_id, replicas, module }`; `scale` → `Scale`; `stop` → `Stop`; `status` → `Status`; `resolve` → `Resolve { uri }`; `upload` → `Upload { manifest, bytes }` with `bytes` read from `--file`. No verb recombines or splits a request.

**Rationale:** The CLI is expressly a thin wrapper over the typed surface — it expresses intent, it does not decompose it. Keeping the mapping identity-preserving guarantees the CLI can never get out of sync with the wire contract.

## Risks / Trade-offs

- **[Two handshakes per command]** Each invocation pays a QUIC handshake plus a bridge channel handshake; a long set of commands is chatty. → Acceptable for an admin CLI; a persistent/shell mode is an explicit non-goal.
- **[Deferred == failure is opinionated]** Until the scheduler's RPC service lands, every `deploy`/`scale`/`stop` will exit non-zero with "scheduler service not yet online". → Deliberate: it is the truthful day-1 status, and the message tells the operator why.
- **[Text output is not machine-readable]** Scripts parsing `sel` output will be brittle. → A `--json` flag is a cheap follow-up; deferred rather than half-designed now.
- **[mTLS material handled by a native process]** The CLI reads and passes client keys but never logs them. → Standard for admin CLIs; nothing is persisted.
- **[Fixture-dependent future e2e]** A later binary-level test needs the `guests/connector-quic/tests/fixtures` client cert (its SPKI is the one the bridge's interim `IdentityGrantMap::stub()` trusts). → The CLI itself is agnostic (presentation only); the interim-trust coupling is a test-harness concern, severed when the identity guest lands.

## Migration Plan

Greenfield crate: add `crates/cli` and its workspace entry, implement, and test at the command-mapping level. No existing code or deployed surface changes.

**Rollback:** remove the workspace member and the crate directory; nothing else references the CLI.
