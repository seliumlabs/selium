# Tasks

## 1. Engine Contract and Feasibility

- [x] 1.1 Confirm the coordinated wasmtiny change delivers concurrent execution of one guest instance across host threads (the `Concurrent Instance Execution Contract`) and verify `openspec validate` passes on that change — without it the worker pool cannot run
- [x] 1.2 Spike: run two host threads entering one guest instance over shared linear memory and verify no data race or store corruption under repeated stress (≥10 consecutive runs)
- [x] 1.3 Engine per-thread global shadowing: the engine gives each concurrent invocation its own `__stack_pointer` shadow stack (resolved from the module's `__stack_pointer` export), so a rustc-compiled module runs over one shared instance without its workers overlapping frames. The mt-demo guest must export it (`scripts/build-all.sh` adds `--export=__stack_pointer`); the wat fixtures do not use a shadow stack and need no export. Verified end-to-end by `sdk_mt_demo_guest_runs_two_cpu_bound_tasks_in_parallel`.

## 2. Guest Executor (Work-Stealing)

- [x] 2.1 Replace the `thread_local!` executor state in `async_runtime.rs` with shared-memory atomics keyed by an explicit worker id and verify `cargo test -p selium-guest` stays green
- [x] 2.2 Implement the shared run queue plus per-task parking words and verify the lost-wakeup stress test passes repeatedly (≥10 consecutive runs)
- [x] 2.3 Convert `JoinState` from `Rc<RefCell>` to `Arc` plus an atomic result slot and verify `JoinHandle` is `Send` and its unit tests pass
- [x] 2.4 Add the `Send` bound to `spawn` and verify the guest crate compiles with it, resolving any `!Send` SDK internals the compiler flags
- [x] 2.5 Emit the worker entry export and verify the module exports both `__selium_guest_poll` (entrypoint exit code) and the worker entry

## 3. Re-Entrant Hostcalls and Wake Addressing

- [x] 3.1 Make the guest hostcall request path re-entrant across workers and verify concurrent hostcalls do not corrupt the request slot under a multi-worker stress test
- [x] 3.2 Add per-task parking-word notify support to the `selium-abi` mailbox/layout and verify the wake-by-notify test passes (a poller wakes a parked task in place)

## 4. Runtime Worker Pool and Wake Delivery

- [x] 4.1 Provision a dedicated OS-thread worker pool per multithreaded guest at spawn and verify the configured worker count starts and joins on teardown
- [x] 4.2 Route wake delivery through notify on the parking word instead of driving the reactor and verify `net_wake --ignored` passes from a poller thread with no embedder pumping
- [x] 4.3 Replace the per-guest execution guard for multithreaded guests while preserving the single-flight task invariant and verify concurrent tasks run with no lost wakes
- [x] 4.4 Extend `module_probe` to detect the multithreaded worker entry export and verify capable and non-capable guests fall back to the correct execution mode

## 5. Fault Isolation

- [x] 5.1 Implement per-worker trap handling (stop the faulted worker, stop the rest, reap the process) and verify a trapping worker tears the guest down without a hang

## 6. Integration

- [x] 6.1 Run the full runtime and kernel suites and verify they are green
- [x] 6.2 Add an end-to-end multithreaded guest fixture and verify two CPU-bound tasks complete in less wall-clock time than serial execution — wat fixtures (`crates/runtime/tests/multithreaded.rs`) plus the real SDK guest (`selium-mt-demo`, `#[ignore]`d, atomics artifact from `scripts/build-all.sh`)
