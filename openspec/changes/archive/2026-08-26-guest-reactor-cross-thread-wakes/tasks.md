# Tasks: Guest Reactor Cross-Thread Wakes

## 1. Decision-gate spike

- [x] 1.1 On a scratch branch, route poller-thread wake sources (`note_generation_advance`, `wake_queue_waiter`) directly through `wake_process_task` (mailbox + inline reactor poll under the execution guard)
- [x] 1.2 Run `net_wake --ignored` ≥10 consecutive passes plus the full `selium-runtime`/`selium-kernel` suites; record pass/failure evidence and the outcome decision in `design.md` — **Outcome A** (10/10 sequential, 12/12 parallel, suites green)

## 2. Wake-path unification (Path A; adapt if Path B)

- [x] 2.1 Delete `pending_exec`, `enqueue_wake_deferred`, and `drain_pending_exec()` from `selium-runtime`; converge all wake sources on `wake_process_task`
- [x] 2.2 Remove `drain_pending_exec()` calls from hostcall boundary hooks and from `net_wake.rs`; document the memory-model contract (execution guard = single-entry invariant) in `process.rs`
- [x] 2.3 Verify guard-held race coverage: a wake arriving while another thread holds the execution guard is delivered via the post-release mailbox re-check (`wake_while_guard_held_is_not_lost`; uses a scratch mailbox + manual guard hold as the deterministic equivalent of a blocked guest)

## 3. Path B only (skip if spike yields Outcome A)

Outcome A confirmed — Path B not needed; tasks cancelled.

- [x] ~~3.1 Introduce per-process `Reactor` struct in `selium-guest`~~ (cancelled)
- [x] ~~3.2 Runtime owns and passes `&mut Reactor` for polls~~ (cancelled)
- [x] ~~3.3 Native unit tests in `async_runtime.rs` updated~~ (cancelled)

## 4. Verification

- [x] 4.1 `net_wake --ignored` passes **without any** `drain_pending_exec()` calls — idle parked guests progress purely on kernel-poller wakes
- [x] 4.2 Concurrency stress test: N threads delivering wakes to one guest while it polls; no lost wakes, no panics, deterministic completion counts (`concurrent_wake_delivery_never_loses_wakes`; also hardened `poll_guest_until_stalled` against unbounded spinning when a poll cannot clear the mailbox)
- [x] 4.3 Long-running embedder example: `main.rs --demo-net-wake` bootstraps `net-demo`, prints its listener address, and serves until Ctrl-C — the service loop contains no wake-delivery or reactor-pumping code; idle guests progress purely on kernel-poller wakes
- [x] 4.4 Full sweep green: `cargo test -p selium-{abi,shm,kernel,guest,runtime}`, wasm build of `net-demo`, clippy clean on touched crates
