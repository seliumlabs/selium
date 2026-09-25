# Proposal

## Why

Selium can quota-enforce memory, storage, pipes, and processes, but it cannot bill CPU or memory *fairly and accurately*: the engine-level metering that once existed in Wasmtiny was removed during the runtime cull, and what remains is a placeholder — `cpu_micros` is fed by a host hook nothing calls in production, the memory gauge is shared-region ownership rather than the guest's own RAM, and the accountant computes a CPU ceiling (`plan + overage`) that it then discards. There is no honest per-tenant CPU or memory measurement at the WASM boundary, so there is no billing-grade accounting.

## What Changes

- **BREAKING** — `MeteringObservation.cpu_micros` is removed and replaced by `cpu_instructions`, the sole CPU quantity: a cumulative, monotonic count of executed guest instructions.
- The host projects per-process instruction counts and a committed-linear-memory-page gauge on the metering tick, retiring the "host instrumentation hook" placeholder.
- The memory gauge is redefined to the guest's committed linear-memory pages (the VM-RAM analogue), distinct from shared-region bytes and durable storage bytes.
- The accountant authors a per-tenant per-minute CPU instruction ceiling (plan + overage); the host halts a process that exhausts its window budget and reaps it through the existing teardown path.
- Metering stays engine-computed and tamper-evident; pricing (a rate card) lives outside the meter and converts instructions to money exactly once.
- Depends on the Wasmtiny change `instance-metering`, which restores the engine-side instruction counter and memory-page gauge.

## Capabilities

### New Capabilities

- `process-metering`: process-level accounting of CPU (executed instructions) and memory (committed linear pages), per-minute billing windows, budget enforcement by halting a process, and the honesty/invariance properties that make the meter billable.

### Modified Capabilities

- `selium-abi`: `MeteringObservation` carries `cpu_instructions` and drops `cpu_micros`.
- `selium-accountant`: counter reduction tracks instructions; the host-projection placeholder is retired in favour of engine-fed instruction and linear-page gauges; the accountant authors per-tenant CPU ceilings each minute.

## Impact

- **Code**: `crates/abi` (observation type), `crates/runtime` (projector, budget enforcement, reap wiring), `crates/service` (FlatBuffers `MeteringBucket`/`TenantPlan` tables regenerate with instruction fields), `guests/accountant` (CPU ceiling authoring).
- **ABI**: `MeteringObservation` loses `cpu_micros` and gains `cpu_instructions` — a breaking change to the observation type, its rkyv encoding, and downstream tests.
- **Dependency**: `wasmtiny` must expose per-instance instruction and memory-page metering (the `instance-metering` change).
