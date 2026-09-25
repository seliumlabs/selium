# Proposal: Discovery Bootstrap Slice

## Why

The spine (merged on the `spine` branch) proves a single WASM guest can
bootstrap, use shared-memory channels, and stream logs. The next cheapest
proof of the platform is the **control plane**: the discovery system guest
running for real inside wasmtiny, wired to the runtime's Tier-1 registration
feed and serving guest RPC lookups.

Two fixed bugs currently block this path, and this slice proves both fixes
end-to-end:

- Entrypoint argument encoding (`set_discovery_handle`/`set_discovery_feed_and_handle`
  used raw u64 bytes; `decode_wasm_arguments` expects tagged `WasmValue`
  bytes, so every discovery-enabled bootstrap previously failed).
- `AttachRegion` now maps into the calling guest's memory, so guests can
  attach channels mid-entrypoint (discovery attaches its feed ring during
  bootstrap).

Without this slice, the control plane remains unproven: discovery is the
only system guest with real wiring, and its feed/RPC surfaces are the
substrate every later system guest (scheduler, supervisor, cluster) hangs
off. It also exercises the two-argument entrypoint path
(`feed_region_id, listener_shared_id`) that no other guest uses.

## What Changes

- Boot the **discovery system guest for real** via
  `Runtime::bootstrap_system_guests` with `start_discovery: true`: runtime
  creates the Tier-1 pub/sub feed ring and RPC listener, injects the feed
  region id and listener handle as tagged entrypoint arguments, and waits
  for readiness.
- Discovery guest attaches the feed ring and listener during its
  entrypoint, marks ready, then services traffic: Tier-1 register/revoke
  events from the feed, and `Resolve`/`Register`/`Revoke` RPC requests from
  other guests.
- Add a second **application guest** (test fixture) that receives the
  discovery handle as its entrypoint argument, connects with
  `Context::from_raw`, registers a Tier-2 URI for a region it owns, and
  resolves it back — proving guest→discovery RPC and ownership validation.
- Prove revocation: when a process exits, its Tier-1 URIs are revoked via
  runtime-published events and lookups stop resolving.

### Explicitly out of scope

- Tenant-aware resolution (`resolve_exact_scoped` stays caller-`None`).
- Prefix lookups over RPC (store supports it; no RPC surface yet).
- Interface metadata ingestion.
- Multi-host discovery, DNS, or durable registration state.

## Capabilities

### New Capabilities

- `discovery-bootstrap`: runtime wiring to boot the discovery guest with
  feed + listener injection, readiness gating, and end-to-end Tier-1 event
  flow and Tier-2 RPC servicing between two real WASM guests.

### Modified Capabilities

- `selium-runtime`: bootstrap with `start_discovery: true` SHALL inject
  tagged entrypoint arguments and gate readiness on the discovery guest's
  own `mark_ready()`; process teardown SHALL publish Tier-1 revocations.

## Impact

- `crates/core/runtime`: bootstrap path (`bootstrap_system_guests`,
  `setup_discovery`), tests.
- `crates/guests/discovery`: no code change expected — the slice proves the
  existing guest; bugs found in the proving are fixed in place.
- `crates/core/runtime/tests/`: new `discovery.rs` integration test with
  two WASM guests (discovery + application fixture).
- Depends on the spine branch fixes (WasmValue argument encoding,
  caller-memory attach, wasmtiny interpreter fixes).
