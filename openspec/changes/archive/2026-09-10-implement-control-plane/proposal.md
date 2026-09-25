## Why

arch3 has no user-facing control plane. The system guests own platform
mechanics (cluster, discovery, scheduling, supervision), but the only
external-facing surface — `selium-external-api` — parses a newline-delimited
text protocol over a raw TCP listener (`deploy w 3`), which DESIGN-INTENT
explicitly rejects ("text protocols for control … typed RPC everywhere"). A
planned external CLI (a thin native wrapper over `selium-client`) needs one
typed, capability-gated control surface that owns user-facing desired state
and delegates platform policy, rather than re-encoding intent as text per
deployment.

## What Changes

- **Repurpose `selium-external-api` into `selium-control-plane`** (crate
  rename; the text-protocol implementation is superseded). **BREAKING** for
  anything consuming the external-api text wire format.
- The control-plane guest **serves a typed RPC surface over a host-queue
  listener** registered as a named service (`control.<tenant>`, derived from
  one `serve` declaration) — the same serving path as discovery. External
  clients reach it through `selium-client` via a **protocol-aware bridge**:
  for host-queue targets the bridge performs the RPC rendezvous on the
  client's behalf (allocate a session region, splice the client's stream into
  it, enqueue the session into the served queue). There is no raw TCP text
  listener.
- The control-plane **owns user-facing desired state** (deployments,
  pipelines, registry-lite). Operations append intent to a durable log and
  project live-table read models; it delegates platform policy — placement to
  `selium-scheduler`, recovery/supervision to `selium-supervisor`,
  naming/resolution to `selium-discovery` — over the existing host-queue RPC
  (`Context`).
- **WASM upload uses the storage capability hostcalls** (`StorageBlobPut` /
  `StorageBlobGet`) directly; there is no storage guest.
- **Access is capability enforcement** using the existing selectors
  (`Tenant` / `ResourceClass` / `ExplicitResource`). The control-plane does
  not re-derive client identity.

## Capabilities

### New Capabilities
- `control-plane`: the control-plane system guest — typed RPC control surface,
  intent model, user-facing desired-state ownership and projection, narrow
  delegation, capability-gated access, and WASM upload via the storage
  hostcalls.

### Modified Capabilities
- `external-api`: requirements replaced — text-protocol parsing and the
  inbound TCP-bridge interface are removed; the guest is superseded by
  `control-plane`.
- `guest-bridge`: the bridge-channel becomes protocol-aware — channel
  targets keep the existing transparent splice; host-queue targets get a
  bridge-side RPC rendezvous (session-region allocation, splice, queue
  enqueue, teardown parity).

## Impact

- `guests/external-api` becomes `guests/control-plane` (package
  `selium-control-plane`); workspace membership is updated alongside the
  other system guests, which are currently frozen out of the workspace.
- `guests/bridge-channel` gains host-queue rendezvous semantics (target-class
  dispatch in `bridge_pipe`); the transparent-splice path for pub/sub and
  live-table targets is unchanged and keeps its regression suite.
- The in-flight `implement-system-guests` change, section 7 (external-api), is
  superseded by this change.
- New FlatBuffers schema bindings shared by the control-plane guest and the
  external client (`schema-wire-generation`).
- `selium-scheduler` must expose the `SchedulerRequest` / `SchedulerResponse`
  RPC service it was already stubbed for (`implement-system-guests` §7.3e
  TODO).
- No CLI crate in this change; the `sel` CLI is a follow-up consuming the
  served surface.
