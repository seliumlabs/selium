# Proposal

## Why

Selium guests today are cooperative, single-flight processes: one host thread enters a guest's WASM store at a time, and a guest's tasks advance one after another on its single reactor. The host runs many guests concurrently across its thread pool, but no single guest can use more than one CPU core — a CPU-bound guest is hard-capped at one core no matter how many cores the host has. This change gives an individual guest genuine CPU parallelism by executing its tasks concurrently on a dedicated pool of OS worker threads over the guest's shared linear memory.

## What Changes

- Add a per-guest pool of dedicated OS worker threads that run a work-stealing async executor over the guest's shared linear memory. Concurrency is host-initiated (the runtime provides the workers); the guest's `spawn`/`JoinHandle` async surface is unchanged and no WASI threads or preemption are introduced.
- Require spawned guest tasks to be `Send` so task capsules can migrate between workers; move guest executor state from `thread_local!`/`Rc<RefCell>` to synchronised shared memory. **BREAKING (guest SDK)**: `spawn` gains a `Send` bound.
- Replace the host's "drive the reactor until it stalls" wake discipline for multithreaded guests with futex delivery: a wake targets the parked task's parking word via atomic notify, and the runtime becomes a scheduler of parked workers rather than the executor of a single reactor.
- Make the guest hostcall path and mailbox re-entrant so multiple workers can call into the host concurrently.
- Isolate per-worker faults: a trap stops only the faulting worker; the host then applies the process teardown policy.
- Keep the existing cooperative single-worker reactor as the default mode; multithreading is opt-in per guest.

## Capabilities

### New Capabilities

- `guest-worker-pool`: the runtime-side execution model for a multithreaded guest — dedicated OS-thread worker provisioning, concurrent task execution over shared linear memory, wake-by-notify delivery, per-worker fault isolation, and the concurrent-instance-execution contract the execution engine must satisfy.

### Modified Capabilities

- `selium-guest`: the reactor parking/wake model relaxes into a concurrent executor — independent runnable tasks may advance in parallel, parked tasks consume no worker, and `spawn` becomes `Send`-bound.
- `channel-wake-wait`: Cross-Thread Wake Delivery is re-expressed for concurrent workers — a wake is delivered by atomic notify on the parked task's parking word, distinct tasks may execute concurrently, and a wake racing an in-flight poll is still never lost.

## Impact

- `crates/runtime`: worker-pool provisioning and lifecycle, notify-based wake delivery, per-worker trap handling.
- `crates/guest`: work-stealing executor, `Send`-bound `spawn`, no-`thread_local!` executor state, re-entrant hostcalls, mailbox → per-task parking words.
- `selium-abi`: parking-word layout and (worker, task) wake addressing primitives.
- Engine (wasmtiny, coordinated change): concurrent execution of one instance across host threads against shared linear memory; the required atomics/wait/notify machinery already exists under the `wasm-threads` capability.
- Specs: new `guest-worker-pool`; modified `selium-guest`, `channel-wake-wait`.
