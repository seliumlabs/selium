# Design: Discovery Bootstrap Slice

## Context

The spine established that a single WASM guest runs the golden path on
wasmtiny. Discovery is a two-argument system guest
(`feed_region_id, listener_shared_id`) whose bootstrap exercises: tagged
`WasmValue` argument encoding, caller-memory `AttachRegion`, the
`__selium_guest_poll` re-entrant wake path (feed subscriber + RPC accept
loops keep the guest reactor alive), and the runtime's discovery feed
publisher (`RuntimeRegionProvider` shm channel).

## Goals / Non-Goals

**Goals:**

- `bootstrap_system_guests(start_discovery: true)` boots discovery plus an
  application guest and both reach readiness.
- Tier-1 events (region alloc/free, process exit) published by the runtime
  are applied by the discovery store in-guest.
- Tier-2 RPC (`Resolve`/`Register`/`Revoke`) works between two real WASM
  guests over the shm RPC rendezvous.
- Revocation on process exit stops URIs resolving.

**Non-Goals:**

- Tenant scoping, prefix RPC, interface metadata, multi-host, durability.
- Discovery guest code changes beyond bug fixes found while proving.

## Decisions

1. **Test fixture as a second guest crate.** Add
   `crates/guests/discovery-probe` (cdylib+rlib, `publish = false`), a
   minimal application guest: takes the discovery handle as its sole
   entrypoint argument, builds `Context::from_raw`, allocates a region,
   registers a Tier-2 URI for it, resolves it, then marks ready. This keeps
   the probe out of `spine-demo` (which stays the minimal golden path).
2. **Bootstrap via the real config path.** The test uses
   `Runtime::bootstrap_system_guests` with `start_discovery: true` and
   descriptors for both guests — no bespoke wiring — so the slice proves
   the same path the (future) CLI uses.
3. **Readiness via `mark_ready()`.** Both guests use
   `ReadinessCondition::ActivityLogContains("guest ready")`; the test then
   drives traffic and asserts on the discovery store via a second probe
   entrypoint or host-side RPC client through the listener queue.
4. **Failure handling: traps are test failures.** Any guest trap or
   readiness timeout fails the test with the activity log drained into the
   panic message (cheap diagnosis; matches spine-test precedent).

## Risks / Trade-offs

- **Re-entrant poll pressure.** Discovery runs accept + feed loops forever;
  `poll_guest_until_stalled` re-enters the guest from hostcall completions
  on other threads. The slice may surface re-entrancy bugs in wasmtiny's
  per-invoke instance model (memories are shared post-spine, but globals/
  tables sync per invoke). Mitigation: keep probe RPC traffic sequential
  and low-volume; file follow-ups for anything found rather than fixing
  the engine in this change.
- **Spin loops.** The discovery guest's feed loop spins on `BufferEmpty`;
  acceptable for the slice (tracked separately by `channel-wake-wait`).
- **Two more wasm fixtures** slow CI slightly; build both in the same CI
  step as `spine-demo`.
