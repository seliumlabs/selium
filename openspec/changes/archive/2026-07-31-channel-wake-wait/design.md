# Design: Channel Wake/Wait

## Context

The shm ring protocol already carries a monotonic **generation counter**
per channel (`GENERATION_COUNTER_OFFSET`, bumped on every frame write).
The reactor already has a mailbox the host uses to wake guest tasks after
hostcall completions. Wake/wait connects these two: waiting on channel
data = waiting on the generation counter to change.

## Goals / Non-Goals

**Goals:**

- No unwakeable `Poll::Pending` anywhere in the I/O stack.
- No spin loops; a parked guest task costs zero CPU.
- `Timer` fires at its deadline.
- Park backpressure actually parks.

**Non-Goals:**

- Cross-host notify, waiter fairness, wait64.
- Eliminating the cooperative reactor (it gains a wake source).

## Decisions

1. **Generation counter is the wait address.** All waitable conditions
   (new data, writer disconnect, slot freed) coincide with generation
   bumps. Writers already bump per frame; disconnect paths will bump on
   writer-count decrement.
2. **Two wait paths, one idiom.**
   - *WASM guests*: `atomic_wait32` on the generation address via
     wasmtiny's `memory.atomic.wait32` (already scaffolded in
     wasmtiny's `Instance::wait32`); `bump_generation` calls
     `memory.atomic.notify`. Where the engine lacks atomic-wait support,
     fall back to a host-mediated signal hostcall (new `SignalWait`
     hostcall: runtime parks the task and wakes it on generation change
     observed via the shared registry) — the fallback keeps guests
     correct, the futex keeps them fast.
   - *Host/native backends* (`KernelBackend`, `HeapRegionProvider`): a
     shared waiters table keyed by (region, offset) with `Notify`-style
     wake, used by kernel proxies and native tests.
3. **Waker registration point**: `poll_read`/`poll_write` capture the
   current generation before evaluating; if the condition is unmet, the
   task registers its `TaskId` in a reactor-side wait map keyed by
   (region_id, generation) and returns Pending. A dedicated reactor
   "pump" (single wait32 per reactor turn, or a host signal) wakes all
   tasks whose generation advanced.
4. **Sleep via a runtime timer wheel**: `SleepWait` entries are armed in a
   runtime-owned min-heap; a single runtime timer thread (or the host's
   poll loop) enqueues mailbox wakes at deadlines. No per-sleep threads.
5. **Spin removal is part of the change, not a follow-up**: every
   `wake_by_ref()`-then-`Pending` site converts to the wait idiom, or the
   pattern isn't proven.

## Risks / Trade-offs

- **wasmtiny atomic-wait support is partial**: `wait32` exists in
  instance.rs but `notify` semantics across guests sharing one region are
  unproven. The host-mediated `SignalWait` fallback derisks the feature;
  futex fast-path lands only where verified.
- **Reactor wake storms**: many tasks on one channel wake together on each
  bump. Acceptable at current scale; a generation-gated re-check on wake
  (tasks re-evaluate before re-parking) keeps it correct.
- **Subtle liveness**: lost-wake risk moves from "everywhere" to one
  place (the wait map); the change includes loom-style stress tests
  (concurrent writer/reader, slot exhaustion churn, timer cancellation).
