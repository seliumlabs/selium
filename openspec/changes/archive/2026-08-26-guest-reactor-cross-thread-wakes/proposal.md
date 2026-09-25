# Proposal: Guest Reactor Cross-Thread Wakes

## Why

`event-driven-net-proxies` introduced a deferred-exec model: kernel
poller threads may only *enqueue* mailbox wakes; the thread that owns a
guest's reactor must call `Runtime::drain_pending_exec()` to execute
them. This is a footgun:

- An embedder that never pumps stalls parked guests silently. The only
  in-repo embedder (`runtime/src/main.rs`) has no service loop at all.
- The contract is implicit and untested at the API surface.

The model was built on the hypothesis that guest reactor state is
thread-affine. That hypothesis is **unverified** — and probably wrong:
on `wasm32-unknown-unknown` (built without the atomics feature),
`thread_local!` lowers to plain statics in linear memory, so the six
reactor cells (`BACKGROUND`, `SPAWN_QUEUE`, `WAKE_QUEUE`,
`CURRENT_TASK`, `NEXT_TASK_ID`, `GEN_WAIT_MAP`) are shared across all
threads executing an instance. wasmtiny holds no thread-locals (the
`Store` is `Arc<Mutex>`); wakers are idempotent task-id markers; the
mailbox lives in linear-memory statics.

## What Changes

- **Decision-gate spike**: empirically establish whether direct
  cross-thread reactor execution (poller thread runs
  `__selium_guest_poll` under the existing execution guard) is correct
  for wasm guests.
- **Path A (reactor not thread-affine — expected)**: delete the
  deferred-exec machinery (`pending_exec`,
  `enqueue_wake_deferred`, `drain_pending_exec`) from the runtime;
  every wake path uses one function that enqueues the mailbox wake and
  executes the reactor inline under the execution guard, regardless of
  calling thread.
- **Path B (reactor is thread-affine)**: move reactor state out of
  thread-locals into runtime-owned per-process storage so that Path A's
  single wake path becomes valid; delete the same machinery.
- Either path removes the embedder cooperation requirement: a guest
  parked on an inbound ring progresses on socket data with **no**
  embedder-side pumping.
- Demonstrate with a long-running embedder example (`main.rs`) in which
  idle guests progress purely on kernel-poller wakes.

## Capabilities

### New Capabilities

(None.)

### Modified Capabilities

- `channel-wake-wait`: cross-thread wake delivery semantics for parked
  tasks.

## Impact

- `selium-runtime`: `process.rs` wake paths, `host_functions.rs`
  boundary hooks; deletion of deferred-exec state.
- `selium-guest` (Path B only): `async_runtime.rs` de-TLS refactor,
  waker plumbing.
- Specs: MODIFIED `channel-wake-wait`.
- Explicitly out of scope: Phase-1-style diagnostics (ownership
  tracking, stale-wake warnings, `Runtime::service`). If the spike
  invalidates Path A, this change expands rather than deferring.
