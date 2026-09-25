# Proposal: Decompose Kernel/Runtime Monoliths and Make Runtime Async

## Why

Two structural issues in `selium-kernel` and `selium-runtime`:

1. **Monolithic structs.** `Kernel` and `Runtime` have `impl` blocks spread across 7 and 5 modules respectively. Every module reaches directly into `KernelInner`/`Runtime` fields with no encapsulation boundaries. The structs are god objects where `process.rs` can lock `shared_mappings`, `hostcall.rs` can touch `loaded_guests`, and nothing prevents it. This isn't idiomatic Rust — it's a flat namespace with file-level grouping.

2. **Sync I/O with OS threads.** Network proxy, accept loops, and timer management all use `std::thread::spawn` with blocking I/O and polling with `thread::sleep`. Per-connection threads don't scale. The kernel already depends on tokio but only uses `Notify` — and that `Notify` is consumed exclusively by dead code (`host_queue_recv` async fn, never called).

Together these issues create a compounding problem: the monolith makes it hard to extract I/O into async tasks because there's no clear subsystem boundary to cut along.

## What Changes

### Kernel: decompose into sub-structs, drop tokio, shed I/O

- **Decompose `KernelInner`** into five sub-structs:
  - `MemoryRegistry` — Store, shared_regions, shared_mappings, id counters
  - `ProcessTable` — processes, activity_log, activity_log_changed, guest_logs, metering, process id counter
  - `StorageRegistry` — durable logs and blob stores
  - `NetworkState` — tcp_listeners, tcp_streams, udp_sockets (state only, no I/O)
  - `HostQueueRegistry` — host queues

- Each sub-struct owns its methods. No `impl Kernel` blocks outside `state.rs` (or entirely removed if sub-structs are the public API).

- **No delegation layer.** The old `impl Kernel { fn start_process(...) { self.processes.start_process(...) } }` forwarding pattern is not created. Sub-structs are accessed directly: `kernel.processes().start_process(...)`.

- **Drop tokio dependency.** Replace `tokio::sync::Notify` with `parking_lot::Condvar` in `HostQueueState` (used only by the dead `host_queue_recv` async fn, which is removed).

- **Network I/O moves to runtime.** All `std::thread::spawn`, blocking socket I/O, and polling loops in `network_runtime.rs` become tokio tasks in a new `selium-runtime::network` module. The kernel's `NetworkState` holds only metadata — the runtime owns live sockets and spawned tasks.

- **Remove dead code:** `Kernel::host_queue_recv` (async, never called).

### Runtime: decompose into sub-structs, become async

- **Decompose `Runtime`** into sub-structs:
  - `GuestTable` — loaded_guests, module_registry
  - `ProcessAuthorityTable` — process_authorities
  - `ResourceTracker` — local_handle_owners, shared_resource_owners, region_purposes
  - `HostcallEngine` — next_operation_id, operations, mailboxes
  - `DiscoveryState` — discovery_publisher, discovery_listener_shared_id

- **Add tokio dependency.** Runtime spawns tokio tasks for network I/O, uses `tokio::time::sleep` for timers.

- **Timer driver replaced.** The dedicated `std::thread` with `mpsc::channel` and `thread::sleep` becomes `tokio::spawn` tasks using `tokio::time::sleep`.

- **Network I/O moved in.** New `network.rs` module owns TCP accept loops, stream proxies, and UDP proxies as tokio tasks.

- The binary crate that embeds the runtime uses `#[tokio::main]`. The runtime library does not spawn its own tokio runtime — it uses `tokio::spawn` from the ambient context.

- **`wait_for_activity_from` stays sync.** Bootstrap is a startup path where blocking is acceptable. The `parking_lot::Condvar` pattern in `ProcessTable` is unchanged.

- Guest polling (`hostcall_poll`) stays sync. WASM execution is synchronous and guests remain cooperatively scheduled.

### Spec updates

Both `selium-kernel` and `selium-runtime` specs need updates:
- Kernel spec: network proxy thread requirements replaced with delegation-to-runtime requirements; UDP/TCP specs updated to reflect runtime-owned I/O.
- Runtime spec: new requirements for async network I/O, tokio-based timer, sub-struct API surfaces.

## Impact

- **Breaking change to public API.** Callers use `kernel.processes().start_process(...)` instead of `kernel.start_process(...)`. All existing call sites in the workspace must be updated.
- **Kernel loses tokio dep.** Cleaner dependency graph.
- **Runtime gains tokio dep.** Already in the workspace dependency tree.
- **Network I/O moves across crate boundary.** `network_runtime.rs` (~860 lines) becomes `runtime/src/network.rs` and is rewritten from threads to tokio tasks.
- **Concurrency model changes.** Tests that relied on `std::thread::sleep` for synchronization against background threads may need `tokio::time::sleep` or different synchronization strategies.

## Risks

- **Test churn.** Many tests create `Kernel`/`Runtime` directly and call their methods. The sub-struct API change affects all of them.
- **Async test complexity.** Tests in runtime that spawn network resources need a tokio context (`#[tokio::test]`).
- **Network rewrite verification.** The TCP/UDP proxy code is nontrivial; rewriting it from threads to async tasks risks introducing subtle timing or ordering bugs.
- **WASM execution stays sync.** The guest execution path is not made async, so hostcall dispatch within guest execution remains synchronous. This is correct but means the async benefits are limited to background I/O, not request/response throughput.
