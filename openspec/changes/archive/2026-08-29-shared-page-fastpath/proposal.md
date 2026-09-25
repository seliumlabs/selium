# Proposal: Shared-Page Wait/Wake Fast Path

## Why

`event-driven-net-proxies` makes the wake graph correct and spin-free
using a portable condvar registry plus guest→host transition kicks. The
kick exists because a guest cannot directly wake a host thread blocked
on a ring word. Investigation of the wasmtiny source shows the engine is
most of the way to closing that gap directly: threads-proposal
validation, `memory.atomic.notify` execution, `shm_open` +
`mmap(MAP_SHARED)` region backing (identical physical pages for host and
guest), and per-region waiter registries all **already exist**. What is
missing is a small public API and the guest-side notify emission — not a
research project.

This is a **progressive enhancement, not a platform dependency** — the
portable condvar path remains correct everywhere, and pursuing this
model does not make Selium Linux-dependent.

## What Changes

Staged, most-value-first:

- **Stage 1 — unified wait registry (portable, no OS-specific code)**:
  wasmtiny exposes a public host-facing wait/notify API on shared
  regions (today `wait_on`/`get_region`/`waiters_arc` are `pub(crate)`),
  and Selium's host-side waits (network proxies) migrate from
  `selium-memory`'s private condvar registry onto wasmtiny's per-region
  registry. A guest's `memory.atomic.notify` then wakes host waiters
  directly; runtime transition kicks are suppressed for those regions.
- **Stage 2 — optional per-OS wait-words**: where wired, host waits use
  the platform primitive (Linux `futex`, macOS `__ulock_*`, Windows
  `WaitOnAddress`/`WakeByAddress`, FreeBSD `_umtx_op`) and wasmtiny's
  notify additionally emits the platform wake. Pure latency/bookkeeping
  win over Stage 1; per-platform opt-in gated by a conformance test.
- **Guest-side notify emission** (the actual driver, previously
  missed): the ring write path emits `atomic_notify` on the generation
  word after a bump, feature-gated on `nightly-wasm-atomics` (already
  scaffolded in `selium-memory`); guests using it declare memory 0
  shared with a maximum (wasmtiny validator requirement). Stable builds
  keep the honest no-op.
- **Support detection, not configuration**: fast-path availability is
  detected per region at attach (engine support flag + guest build
  feature); fallback to the portable kick path is automatic and silent.

## Capabilities

### New Capabilities

- `shared-page-fastpath`: the unified shared-region wait registry,
  guest notify emission, optional per-OS wait-words, and the
  detection/fallback honesty rules

### Modified Capabilities

(None — the `channel-wake-wait` portable path is unchanged and remains
the fallback.)

## Impact

- wasmtiny (separate repo): public host wait/notify API on shared
  regions; support flag; optional platform wake emission. This change
  pins the contract and integrates against it.
- `selium-shm`: notify emission on the write path (feature-gated).
- `selium-memory`: host backend delegation to the unified registry;
  optional per-OS wait-words.
- `selium-runtime`: kick suppression for fast-path regions.
- `selium-abi`: no changes.
- Depends on: `event-driven-net-proxies` (it optimises machinery that
  change introduces).
- Non-goals: making the fast path mandatory; cross-host wakes;
  `atomic.wait64`.
