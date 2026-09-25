# Proposal: Implement the Accounting Guest for Selium arch3

## Why

The platform meters cpu, memory, storage, and bandwidth per process, but nothing aggregates that into per-tenant usage, compares it against paid entitlements, or enforces limits. The accounting guest adds the resource-driven revenue control loop: a runtime-local bookkeeper reduces per-process observations into per-tenant buckets, and a global accountant turns those into billing, quota, narrow-ing, and rate-limit state enforced preemptively at the host and edge.

## What Changes

- **New `selium-accountant` system guest** (`crates/guests/accountant`, package `selium-accountant`), exposing two entrypoints from one module:
  - **bookkeeper** (one per host): polls `MeteringRead` for local processes on a 1-second cadence, differences cumulative counters (cpu/bandwidth), samples gauges (memory/storage), and publishes per-tenant buckets to a shared-memory topic.
  - **accountant** (one logical instance): merges bookkeeper buckets, rolls per-minute billing windows into a durable usage ledger, evaluates tenant account state against plan + opt-in overage budget, and writes enforcement state.
- **Quota-primitive framing**: quotas are a host-held counter table distinct from capability grants — `QuotaSet`/`QuotaClear` hostcalls authored by the accountant and enforced synchronously at allocation (shared memory, storage, queued pipe items), with denials returning a distinct `QuotaExceeded` error code naming the tenant and resource class. A handed-off resource transfers ownership and quota reservation to the receiver.
- **Revenue ceilings**: soft ceiling = plan (meter + bill overage), hard ceiling = plan + overage budget (preemptive deny/throttle at the chokepoints).
- **Reactive account state machine**: paid / in-overage / at-budget / delinquent, each mapped to knob values (quotas), and narrowing for the bridge, driven by observed usage and operator/billing transitions. Bandwidth rate bands and pricing are operator-authored policy — their authoring surface (cloud management tooling) and the connector's consumption are deferred.
- **Single-host v1**: bookkeeper and accountant co-locate over shared memory; cross-host fan-in of bucket feeds is designed but deferred to the platform's cross-host routing work.

## Capabilities

### New Capabilities

- `selium-accountant`: the bookkeeper and accountant entrypoints — per-second metering reduction into per-tenant buckets, per-minute billing windows and the durable usage ledger, plan/overage ceiling evaluation, tenant account-state transitions, and authoring of quota and narrowing enforcement state. Bandwidth rate bands and pricing are operator-authored policy (their authoring surface is future cloud management tooling) and are out of scope here.

### Modified Capabilities

- `selium-abi`: adds the `QuotaWrite` capability variant, the `QuotaSet` / `QuotaClear` hostcall request and output variants, and the `QuotaExceeded` error code for quota denials.
- `capability-enforcement`: `QuotaWrite` is bootstrap-provisioned and not conferable at spawn, mirroring `MintCertificate` and `DelegateGrants`.
- `guest-bridge`: conferral folds the accountant's narrowing state into the identity-published baseline grants before spawning a bridge-channel.

## Impact

- **New crate**: `crates/guests/accountant` (`selium-accountant`).
- **`crates/abi`**: new `QuotaWrite` capability variant, new `HostcallRequest`/`HostcallOutput` variants.
- **`crates/runtime`**: host-held quota table consulted at allocation (shared memory bytes, storage bytes, queued pipe items), `QuotaWrite` admission, metering producer ticker (per-second projection into the kernel, with TCP bandwidth instrumentation feeding the projector; per-process CPU accounting is a placeholder hook until the WASM-resources work), handoff ownership transfer with quota reservation following the resource, quota release at process teardown (storage stays sticky — durable bytes persist by design; a user-side storage-management mechanism, e.g. a CLI capability, is TBA), bookkeeper/accountant bootstrap descriptors with dependency-ordered readiness.
- **`crates/kernel`**: quota counter state alongside the existing per-process metering table.
- **`guests/bridge`**: read the accountant's narrowing table and confer `baseline ⊖ narrowing`.
- **`guests/connector-quic`**: consume operator-authored rate bands (deferred with the cloud management tooling; v1 quota enforcement is host-side only).
- **`selium-service`**: bookkeeper bucket and accountant control message types (additive).
