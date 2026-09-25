# Design: Guest Reactor Cross-Thread Wakes

## Context

`event-driven-net-proxies` landed a deferred-exec model after an
observed failure in which a poller-thread reactor poll "did nothing".
Subsequent analysis shows that observation is fully explained by an
unrelated bug present at the time (the accept operation had already
transitioned to `Failed`, so re-polls could never progress). The
thread-affinity hypothesis behind deferred-exec was never isolated and
tested. Static analysis cuts against it:

- On `wasm32-unknown-unknown` without `nightly-wasm-atomics`,
  `thread_local!` lowers to ordinary linear-memory statics with one-time
  lazy initialisation — shared by every thread executing the instance.
- wasmtiny has no thread-locals; the interpreter `Store` is
  `Arc<Mutex<Store>>`.
- The mailbox is `static mut MAILBOX` in linear memory; wakers are
  idempotent `TaskWake(task_id)` markers into shared queues.
- The only true concurrency hazard — two threads inside WASM at once —
  is already excluded by the runtime's execution guard plus exclusive
  removal of `LoadedGuest` from the registry during polls.

## Goals / Non-Goals

**Goals:**

- One wake path: any thread that observes a wake condition can deliver
  it end-to-end (mailbox + reactor poll) without cooperation from an
  "owning" thread.
- No embedder-side pumping required for correctness: idle guests parked
  on rings/queues progress on kernel wakes alone.
- Deletion of `pending_exec`, `enqueue_wake_deferred`,
  `drain_pending_exec`.

**Non-Goals:**

- Multi-instance, multi-threaded wasm (shared-everything threads).
- Changing the guest SDK's public async surface (`spawn`,
  `TcpStream`, …) beyond what Path B internally requires.
- Timer/sleep delivery redesign.

## Decision Gate (Spike)

A throwaway experiment on a scratch branch, before implementation:

1. Revert `enqueue_wake_deferred` call sites to direct
   `wake_process_task` (mailbox enqueue + inline reactor poll) from the
   poller thread, keeping the execution guard.
2. Run `net_wake --ignored` plus the full runtime/kernel suites,
   repeated (≥10 runs) to expose races.

**Outcome A (passes):** reactor state is effectively shared on the wasm
path → implement Path A.
**Outcome B (fails / flaky):** isolate the failing mechanism (assert on
`RefCell` borrow panics, instrument static addresses across threads);
implement Path B.

### Spike result: OUTCOME A (2025-08, branch `spike/reactor-cross-thread-wakes`)

Changes: `note_generation_advance` and `wake_queue_waiter` route
through `wake_process_task` directly — mailbox wake + inline reactor
poll executed **on the kernel poller thread** whenever the execution
guard is free. No deferral.

Evidence:

- `net_wake --ignored`: **10/10 consecutive passes** (~2.1 s each).
  Every pass exercises poller-thread inline WASM execution three ways:
  accept (`HostQueueRecvWait` wake), data arrival
  (`WaitRegister`/generation wake), and EOF generation bump.
- Full `selium-runtime` + `selium-kernel` suites: green (37 lib tests,
  all integration tests).
- Parallel stress: 4 concurrent test processes × 3 rounds = **12/12
  passes**, scrambling poller/reactor/hostcall interleavings across
  processes. (An initial parallel batch failed, but the cause was the
  stress harness resolving the wasm fixture path relative to CWD — not
  concurrency.)

Conclusion: guest reactor state behaves as shared linear-memory statics
on the wasm execution path, as predicted by TLS lowering for
`wasm32-unknown-unknown`. Cross-thread reactor execution under the
existing execution guard is correct. Implement **Path A**.

## Path A — Inline cross-thread wakes (expected)

- `note_generation_advance`, `wake_queue_waiter`, and all other wake
  sources converge on `wake_process_task(process_id, task_id)`
  (mailbox enqueue + `poll_guest_until_stalled` under the guard).
- Delete `pending_exec`, `enqueue_wake_deferred`,
  `try_begin_guest_exec`'s defer semantics stay (guard remains — it is
  the mutual-exclusion mechanism, not an affinity mechanism).
- Remove `drain_pending_exec()` from the public API and its calls at
  hostcall boundaries; host functions keep only
  `kick_network_waiters()`.
- Memory-model contract documented in `process.rs`: single-entry
  invariant via execution guard; linear-memory statics accessed only
  under that invariant; mailbox flag handshake unchanged.

### Honesty requirements

- No fake success: if the mailbox enqueue fails, the wake is not
  delivered (existing behaviour preserved).
- Spurious wakes remain harmless: reactors re-check state after wake
  (futex discipline already in place).
- A wake delivered while another thread holds the execution guard must
  not be lost: the guard holder re-checks pending mailbox state after
  release (already implemented in `poll_guest_until_stalled`).

## Path B — De-TLS fallback (only if spike fails)

- Introduce `struct Reactor` in `selium-guest` holding
  `background: Vec<BackgroundTask>`, `spawn_queue`, `wake_queue`,
  `current_task`, `next_task_id`, `gen_wait_map`.
- Runtime owns one `Reactor` per process alongside `LoadedGuest`; guest
  crate fns take `&mut Reactor` (wasm entry shims fetch it from a
  single static instead of TLS).
- Wakers become `Arc<TaskWake>` as today but resolve through the same
  per-process storage; `JoinHandle` keeps `Rc<RefCell>` (never sent
  across threads).
- Then apply Path A's wake-path simplification on top.

## Risks / Trade-offs

- **Spike may be inconclusive under load**: mitigate with repetition,
  stress test (see tasks), and TSAN-style logging of guard acquisitions
  across threads.
- **Path B scope creep**: waker plumbing through `&mut Reactor` touches
  most of `async_runtime.rs`; time-boxed, and Path A remains the target
  outcome.
- **Removing `drain_pending_exec()` breaks the one new caller**
  (`net_wake.rs`) — updated in the same change; no external embedders
  exist yet, so now is the cheapest moment.
