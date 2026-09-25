# Design

## Context

See proposal.md — Why for motivation. The current model enforces "one guest = one flight" at three layers: the guest SDK's `thread_local!`/`Rc<RefCell>` state (`async_runtime.rs`), the runtime's `executing_guests` guard plus exclusive `LoadedGuest` removal during polls (`runtime.rs`, `process.rs`), and the engine's `call_function(&mut self)` over `Arc<Mutex<...>>` store/memory/tables. The engine (wasmtiny) already mmap-backs guest linear memory, accesses it natively on the AOT path, and implements atomic wait/notify on shared ranges under the `wasm-threads` capability; the missing engine piece — concurrent execution of one instance across host threads — is the coordinated wasmtiny change.

The guest SDK is already mostly `Send`: `selium-shm` send/receive return `impl Future + Send`, regions carry `AtomicU64` state, and the `Rc`/`RefCell`/`thread_local!` surface is concentrated in `async_runtime.rs`, `platform.rs`, and `log.rs`.

## Goals / Non-Goals

**Goals:**

- CPU parallelism within one guest while preserving the `spawn`/`JoinHandle` async surface.
- Keep the single-worker cooperative reactor as the default; multithreading is opt-in per guest.
- Wake by notify, never by re-running the whole reactor on the waking thread.

**Non-Goals:**

- WASI threads, `std::thread` in the guest, or preemption (model A).
- A guest-visible threading API (no `thread.spawn`).
- Changing the pub/sub, RPC, channel, or live-table API surface.

## Decisions

**1. Work-stealing async executor inside wasm (model B), not WASI threads.**
Preserves the async SDK surface and matches the archived cross-thread-wakes non-goal ("don't change the SDK's public async surface"). Concurrency is host-initiated: the runtime provides N workers; each runs the guest's worker entry over shared linear memory. Alternative (model A) was rejected: it requires a WASI-threads target, breaking the project's `wasm32-unknown-unknown`-only constraint.

**2. Dedicated OS worker threads, count = min(cores, host cap, config).**
AOT executes wasm natively, so each worker is a real OS thread rather than a tokio blocking task; this also keeps guest compute off tokio's I/O workers. Default is bounded by available cores; a host config spread caps or raises it.

**3. No `thread_local!` in the executor.**
On this target `thread_local!` lowers to shared linear-memory statics. Worker identity is passed explicitly as the worker entry's argument, and all shared executor state (run queue, task table, parking words) lives in atomics, not TLS.

**4. `Send`-bound `spawn`; `JoinState` becomes `Arc` + atomic slot.**
Task capsules must migrate for stealing, so the runtime future graph must be `Send`. The ~68-line `Rc`/`RefCell` surface (`async_runtime.rs`, `platform.rs`, `log.rs`) is the bounded migration set.

**5. Wake discipline: bump the task's parking word, notify the shared wake word.**
For multithreaded guests the host never "drives the reactor to stall"; it bumps the task's host-visible parking word (an observable mirror of the task's wake counter) and notifies the guest's shared wake word, and a parked worker resumes in place — the woken task is claimed by whichever worker takes it from the shared run queue (tasks migrate; there is no task-to-worker binding). Wake-racing is the standard re-check-before-park: the polling worker re-checks pending wake state before parking (mirrors the existing futex discipline in `poll_guest_until_stalled`). The single-worker mailbox path is retained for cooperative-mode guests, so existing requirements like Cross-Process Wait Registration stay valid.

**6. Re-entrant hostcalls.**
Each worker stages its hostcall request in a per-worker slot; the runtime's operation registry is already keyed by `OperationId` and thread-safe. The mailbox's single-producer/single-consumer handshake is replaced by per-task parking words for multithreaded guests.

**7. Per-worker fault isolation.**
A faulted worker unwinds and stops; the runtime stops the remaining workers and reaps the guest, extending `record_poll_failure`'s trap path. Cleanup reuses the idempotent `cleanup_failed_process`.

**8. Three de-risking spikes before the full build.**
(1) Engine SMP feasibility in wasmtiny; (2) executor lost-wakeup correctness; (3) `Send` audit of the guest future graph.

## Risks / Trade-offs

- **[Engine SMP is the long pole]** → wasmtiny spike first; this change's tasks declare the dependency on the coordinated engine change.
- **[Executor memory-ordering / lost-wakeup bugs]** → spike 2 plus stress tests; futex discipline already proven in-tree for rings.
- **[`Send` bound breaks existing guests using `Rc`/`!Send` state]** → opt-in rollout: system guests stay single-worker until migrated; `Send` audit is spike 3.
- **[Worker oversubscription vs tokio]** → dedicated OS threads with a core-bound default cap.
- **[Trap-teardown races]** → stop-then-join with idempotent cleanup.

## Migration Plan

1. Land the coordinated wasmtiny SMP change first (satisfies the Concurrent Instance Execution Contract).
2. Ship this change behind capability detection: extend `module_probe` to recognise the multithreaded worker entry export, so unconverted guests fall back to the single-worker reactor.
3. Migrate CPU-bound system guests one by one through the `Send` audit.
4. No shared-artifact rollback needed; multithreading is per-guest opt-in.

## Engine Contract: Per-Invocation Shadow Stacks

A rustc-compiled module keeps every function frame on a single
`__stack_pointer` shadow stack in linear memory, so concurrent invocations of
one shared instance would overlap frames unless each gets its own stack. The
coordinated engine change therefore gives every concurrent invocation a
**private shadow stack**: when the module exports `__stack_pointer` (the
wasm-threads convention), `invoke_shared` runs it with a per-invocation globals
copy whose stack-pointer cell addresses a private stack slot carved from the
committed memory. Ordinary mutable globals stay shared (copied back on exit);
only the stack pointer is per-invocation.

Consequence for the guest build: a multithreaded guest must export
`__stack_pointer`, which `scripts/build-all.sh` arranges with
`-C link-arg=--export=__stack_pointer`. The `module_probe` gate already selects
multithreaded execution by the `__selium_guest_worker` export, so every
unconverted guest stays on the cooperative reactor.

Known follow-up: real per-thread `__tls_base`/`__wasm_init_tls` TLS is not
engine-implemented, so a `__tls_base`-relative thread-local is shared across
invocations. The SDK's executor keeps no cross-worker state in TLS, so this
does not affect the worker pool.

## Open Questions

- Default pool-size policy beyond "min(cores, host cap)": whether a host-wide cap is configured per cluster or per runtime. Recorded assumption: per-runtime config, core-bound default.
- Whether the single-worker mailbox path is eventually retired or kept alongside the notify path indefinitely. Recorded assumption: keep both until multithreaded mode is proven in production.
