# Tasks: Decompose Monoliths and Make Runtime Async

## Phase 1: Kernel Sub-Structs (no behavior change)

- [x] **1.1** Extract `MemoryRegistry` from `KernelInner`. Move `store`, `shared_regions`, `shared_mappings`, `next_local_id`, `next_shared_id` into new struct. Move `memory.rs` methods to `impl MemoryRegistry`. Keep `id_seed` on `KernelInner`; pass to `MemoryRegistry::new(seed)`.
- [x] **1.2** Extract `ProcessTable` from `KernelInner`. Move `processes`, `activity_log`, `activity_log_changed`, `guest_logs`, `metering`, `next_process_id` into new struct. Move `process.rs` methods to `impl ProcessTable`. Pass `id_seed` at construction.
- [x] **1.3** Extract `StorageRegistry` from `KernelInner`. Move `durable_logs_by_shared`, `local_logs`, `blob_stores_by_shared`, `local_blob_stores` into new struct. Move `storage.rs` methods to `impl StorageRegistry`. Needs `&MemoryRegistry` for id generation.
- [x] **1.4** Extract `NetworkState` from `KernelInner`. Move `tcp_listeners`, `tcp_streams`, `udp_sockets` into new struct. Move close/insert methods. Keep `network_runtime.rs` I/O methods on `Kernel` for now (Phase 3 moves them).
- [x] **1.5** Extract `HostQueueRegistry` from `KernelInner`. Move `host_queues_by_shared`, `local_host_queues` into new struct. Move `host_queue.rs` sync methods to `impl HostQueueRegistry`.
- [x] **1.6** Update `Kernel` public API. Remove direct `impl Kernel` blocks from all modules except `state.rs`. Add accessor methods returning Clone handles to each sub-struct. Update all internal callers.
- [x] **1.7** Update all workspace call sites for new `kernel.processes().start_process(...)` API. This includes `selium-runtime` (all modules), kernel integration tests, and any other crate using `Kernel` directly.
- [x] **1.8** Remove dead code: `Kernel::host_queue_recv` async fn.
- [x] **1.9** Drop tokio dependency from `selium-kernel`. Replace `tokio::sync::Notify` in `HostQueueState` with `parking_lot::Condvar`. Update `close_tcp_listener` to use condvar.

## Phase 2: Runtime Sub-Structs (no behavior change)

- [x] **2.1** Extract `GuestTable` from `Runtime`. Move `loaded_guests`, `module_registry` and their methods.
- [x] **2.2** Extract `ProcessAuthorityTable` from `Runtime`. Move `process_authorities` and authority methods (`persist`, `restore`, `tenant`, `set_parent`).
- [x] **2.3** Extract `ResourceTracker` from `Runtime`. Move `local_handle_owners`, `shared_resource_owners`, `region_purposes` and their methods (`claim_local_handle`, `release_local_handle`, `claim_shared_resource`, `release_shared_resource`, `ensure_*_owner`, region purpose methods).
- [x] **2.4** Extract `HostcallEngine` from `Runtime`. Move `next_operation_id`, `operations`, `mailboxes` and their methods (`begin_hostcall`, `poll_hostcall`, `drop_hostcall`, `next_operation_id`, `register_mailbox`, `wake_process_task`).
- [x] **2.5** Extract `DiscoveryState` from `Runtime`. Move `discovery_publisher`, `discovery_listener_shared_id` and their methods (`setup`, `publish`, accessors).
- [x] **2.6** Update `Runtime` public API. Remove direct `impl Runtime` blocks from process.rs, hostcall.rs, bootstrap.rs, host_functions.rs. Add accessor methods for each sub-struct. Convert `dispatch_hostcall`, `cleanup_process_resources`, `cleanup_failed_process`, `bootstrap_system_guests`, `spawn_system_guest`, `register_runtime_host_functions`, `wake_host_queue_waiters` to free functions taking sub-struct refs.
- [x] **2.7** Update all internal runtime call sites for the new sub-struct API.

## Phase 3: Move Network I/O from Kernel to Runtime

- [x] **3.1** Add tokio dependency to `selium-runtime` with features: `net`, `rt-multi-thread`, `sync`, `time`.
- [x] **3.2** Create `runtime/src/network.rs` with async versions of `tcp_bind`, `tcp_connect`, `udp_bind`.
- [x] **3.3** Implement async `accept_loop` using `tokio::net::TcpListener::accept().await`.
- [x] **3.4** Implement async `run_tcp_proxy` using `tokio::net::TcpStream` split with `tokio::io::split` and `read().await`/`write_all().await`.
- [x] **3.5** Implement async `run_udp_proxy` using `tokio::net::UdpSocket` with `recv_from().await`/`send_to().await`.
- [x] **3.6** Port `create_stream_region` helper from kernel to runtime (uses `MemoryRegistry`, no I/O change).
- [x] **3.7** Update `dispatch_hostcall` to call runtime's async network methods instead of kernel's sync ones. `TcpBind`, `TcpConnect`, `UdpBind` now spawn tokio tasks.
- [x] **3.8** Remove `network_runtime.rs` from kernel. Remove the old `impl Kernel` blocks for `tcp_bind`, `tcp_connect`, `udp_bind`, `close_tcp_listener`, `close_tcp_stream`, `close_udp_socket` and all helper functions (`proxy_inbound`, `proxy_outbound`, `run_proxy`, `run_udp_proxy`, `tcp_accept_loop`, `udp_proxy_recv`, `udp_proxy_send`, `create_stream_region`, `ring_err`).
- [x] **3.9** Update `cleanup_process_resources` in runtime to use `NetworkState` methods directly instead of kernel `close_*` methods.
- [x] **3.10** Move kernel network tests to runtime. Annotate with `#[tokio::test]`.

## Phase 4: Async Timer

- [x] **4.1** Replace timer driver thread with `tokio::spawn` in `dispatch_hostcall` for `Sleep { millis }`.
- [x] **4.2** Remove `timer_tx` field from `HostcallEngine`, remove `TimerRequest` type, remove `register_timer` method.
- [x] **4.3** Remove `std::sync::mpsc::channel` usage from `Runtime::new`.
- [x] **4.4** Update timer-related tests.

## Phase 5: Cleanup and Validation

- [x] **5.1** Update kernel spec in `openspec/specs/selium-kernel/spec.md` to match new sub-struct architecture and removed network I/O requirements.
- [x] **5.2** Update runtime spec in `openspec/specs/selium-runtime/spec.md` to match new sub-struct architecture and async requirements.
- [x] **5.3** Run full workspace test suite (`cargo test --workspace`). All tests pass.
- [x] **5.4** Run clippy and fix warnings. Clean.
- [x] **5.5** Ensure `#[tokio::main]` is on the binary entry point. Added `#[tokio::test]` to all runtime tests that spawn network resources or sleep timers.
