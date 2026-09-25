# Design: Decompose Kernel/Runtime Monoliths and Make Runtime Async

## Sub-Struct Boundaries

### Kernel (`selium-kernel`)

```
┌──────────────────────────────────────────────────────────────────┐
│  Kernel                                                          │
│  ┌────────────────────────────────────────────────────────────┐  │
│  │  KernelInner {                                              │  │
│  │    memory: MemoryRegistry,                                   │  │
│  │    processes: ProcessTable,                                  │  │
│  │    storage: StorageRegistry,                                 │  │
│  │    network: NetworkState,                                    │  │
│  │    queues: HostQueueRegistry,                                │  │
│  │    id_seed: u64,  // read-only after construction            │  │
│  │  }                                                           │  │
│  └────────────────────────────────────────────────────────────┘  │
└──────────────────────────────────────────────────────────────────┘
```

#### MemoryRegistry
```
MemoryRegistry {
    store: Mutex<Store>,
    shared_regions: Mutex<HashMap<SharedResourceId, SharedRegionRecord>>,
    shared_mappings: Mutex<HashMap<u64, SharedMappingState>>,
    next_local_id: AtomicU64,
    next_shared_id: AtomicU64,
}
```
Owns: `allocate_shared_region`, `attach_shared_region`, `destroy_shared_region`, `detach_shared_region`, `detach_all_shared_mappings`, `read_shared_memory`, `write_shared_memory`, `fetch_add_shared_memory_u64`, `compare_exchange_shared_memory_u64`, `shared_region_len`, `shared_mapping_shared_id`, `shared_region_mapping_count`, `wasmtiny_region_id`, `attach_shared_region_to_memory`, `register_guest_allocated_region`, `shared_store`.

`id_seed` is passed from `KernelInner` to `MemoryRegistry` (and `ProcessTable`, `HostQueueRegistry`) at construction. Each sub-struct generates its own id space using hashed_id with its own atomic counter.

#### ProcessTable
```
ProcessTable {
    processes: Mutex<HashMap<ProcessId, ProcessState>>,
    activity_log: Mutex<Vec<ActivityEvent>>,
    activity_log_changed: Condvar,
    guest_logs: Mutex<Vec<GuestLogEntry>>,
    metering: Mutex<HashMap<ProcessId, MeteringObservation>>,
    next_process_id: AtomicU64,
}
```
Owns: `start_process`, `stop_process`, `reap_process`, `inspect_process`, `record_activity`, `read_activity_from`, `wait_for_activity_from`, `write_guest_log`, `read_guest_logs_from`, `register_log_channel`, `log_channel_shared_id`, `drain_log_channel`, `process_grants`, `observe_metering`, `metering_observation`.

Note: `register_log_channel` and `drain_log_channel` need a `&MemoryRegistry` to create/use `KernelBackend`. They take it as a parameter.

#### StorageRegistry
```
StorageRegistry {
    durable_logs_by_shared: Mutex<HashMap<SharedResourceId, DurableLogState>>,
    local_logs: Mutex<HashMap<u64, SharedResourceId>>,
    blob_stores_by_shared: Mutex<HashMap<SharedResourceId, BlobStoreState>>,
    local_blob_stores: Mutex<HashMap<u64, SharedResourceId>>,
}
```
Owns: `open_log`, `append_log`, `replay_log`, `checkpoint_log`, `checkpoint_sequence`, `open_blob_store`, `put_blob`, `get_blob`, `set_manifest`, `get_manifest`, `close_log`, `close_blob_store`, `log_shared_id`, `blob_store_shared_id`.

`open_log`/`open_blob_store` need `next_shared_id`/`next_local_id` — these come from the appropriate registries (MemoryRegistry for shared IDs, StorageRegistry gets its own local-ID counter). Wait — currently `open_log` calls `self.next_shared_id()` and `self.next_local_id()` which live on `Kernel`. Each sub-struct that needs id generation gets its own counter. But `next_shared_id` is already shared between MemoryRegistry, StorageRegistry, HostQueueRegistry, and the old Kernel (process ids are separate). These are different id spaces — they use the same hash function but check against different maps for collisions.

Design decision: each sub-struct that generates IDs gets its own `AtomicU64` counter. The `id_seed` is shared (passed at construction). Since each sub-struct checks its own maps for collisions, there's no cross-subsystem conflict — a shared_id from MemoryRegistry won't collide with a shared_id from StorageRegistry because they're stored in different maps.

But wait — `open_log` currently calls `self.next_shared_id()` which produces IDs used as keys in `durable_logs_by_shared`. And `allocate_shared_region` also calls `self.next_shared_id()` for keys in `shared_regions`. Both maps use `SharedResourceId` as keys. Could there be a collision? Currently no — both use the same `next_shared_id` counter, so IDs are unique across both maps. With separate counters per sub-struct, two sub-structs could produce the same ID.

This matters. The fix: either (a) use a shared ID generator, or (b) use type-state to distinguish IDs. Option (a) is simpler: MemoryRegistry owns the shared ID counter, and other sub-structs call `memory.next_shared_id()` when they need one. But that creates coupling...

Actually, looking more carefully: `open_log` creates a `shared_id` and stores it in `durable_logs_by_shared`. `allocate_shared_region` creates a `shared_id` and stores it in `shared_regions`. These are different maps. If both sub-structs generate IDs independently, there's no runtime collision within the same map but there could be confusion if someone passes a log's shared_id to `attach_shared_region`. 

Simplest fix: **keep a single shared-id counter on `KernelInner`** that all sub-structs use. It's just an `AtomicU64` — one field, not worth over-engineering. Same for local IDs? Actually local IDs are per-map too (shared_mappings, local_logs, local_blob_stores, local_host_queues). But same concern applies.

Better approach: `MemoryRegistry` owns both `next_shared_id` and `next_local_id` counters. Other sub-structs that need IDs call `memory.next_shared_id()` or `memory.next_local_id()`. This is explicit: MemoryRegistry is the source of all kernel IDs.

#### NetworkState
```
NetworkState {
    tcp_listeners: Mutex<HashMap<u64, TcpListenerMetadata>>,
    tcp_streams: Mutex<HashMap<SharedResourceId, TcpStreamMetadata>>,
    udp_sockets: Mutex<HashMap<SharedResourceId, UdpSocketMetadata>>,
}
```
Where the metadata types contain only `shared_id` and `running: Arc<AtomicBool>` — no live OS sockets.

Owns: `insert_tcp_listener`, `remove_tcp_listener`, `insert_tcp_stream`, `remove_tcp_stream`, `insert_udp_socket`, `remove_udp_socket`, and the `close_*` methods (which set `running=false` and remove entries).

The actual TcpListener/UdpSocket/TcpStream objects live in the runtime's async tasks.

#### HostQueueRegistry
```
HostQueueRegistry {
    queues_by_shared: Mutex<HashMap<SharedResourceId, Arc<HostQueueState>>>,
    local_queues: Mutex<HashMap<u64, SharedResourceId>>,
}
```
Where `HostQueueState.entries` remains `Mutex<VecDeque<(u64, u64)>>` but `notify` changes from `tokio::sync::Notify` to `parking_lot::Condvar`.

Owns: `create_host_queue`, `attach_host_queue`, `host_queue_send`, `try_host_queue_recv`, `host_queue_shared_id`.

Dead code removed: `host_queue_recv` (async fn, never called).

### Runtime (`selium-runtime`)

```
┌──────────────────────────────────────────────────────────────────┐
│  Runtime                                                         │
│  ┌────────────────────────────────────────────────────────────┐  │
│  │  Runtime {                                                  │  │
│  │    kernel: Kernel,                                          │  │
│  │    guests: GuestTable,                                      │  │
│  │    authorities: ProcessAuthorityTable,                      │  │
│  │    resources: ResourceTracker,                              │  │
│  │    hostcalls: HostcallEngine,                               │  │
│  │    discovery: DiscoveryState,                               │  │
│  │  }                                                           │  │
│  └────────────────────────────────────────────────────────────┘  │
└──────────────────────────────────────────────────────────────────┘
```

#### GuestTable
```
GuestTable {
    loaded_guests: Mutex<HashMap<ProcessId, LoadedGuest>>,
    module_registry: Mutex<HashMap<String, Vec<u8>>>,
}
```
Owns: `insert`, `remove`, `get`, `entrypoint_results`, `loaded_count`, `register_module_bytes`, `module_bytes`.

#### ProcessAuthorityTable
```
ProcessAuthorityTable {
    authorities: Mutex<HashMap<ProcessId, ProcessAuthority>>,
}
```
Owns: `insert` (was `persist_process_authority`), `remove`, `get` (was `restore_process_authority`), `get_grants`, `tenant`, `set_parent`.

#### ResourceTracker
```
ResourceTracker {
    local_handle_owners: Mutex<LocalHandleOwners>,
    shared_resource_owners: Mutex<SharedResourceOwners>,
    region_purposes: Mutex<RegionPurposes>,
}
```
Owns: `claim_local_handle`, `release_local_handle`, `ensure_local_handle_owner`, `claim_shared_resource`, `release_shared_resource`, `ensure_shared_resource_owner`, `set_region_purpose`, `take_region_purpose`, `shared_region_owners`.

The `cleanup_process_resources` and `cleanup_failed_process` methods become functions that take `&Runtime` (or `&ResourceTracker`, `&Kernel`, `&DiscoveryState`) as parameters, since they orchestrate across multiple sub-structs.

#### HostcallEngine
```
HostcallEngine {
    next_operation_id: Mutex<OperationId>,
    operations: Mutex<HashMap<OperationId, HostOperation>>,
    mailboxes: Mutex<HashMap<ProcessId, Arc<GuestMailbox>>>,
}
```
Owns: `begin`, `poll`, `drop_hostcall`, `next_operation_id`, `register_mailbox`, `wake_process_task`.

The `dispatch_hostcall` method moves to the hostcall module as a function that takes `&Runtime` (needs kernel, resources, authorities, discovery). Same for `wake_host_queue_waiters`.

#### DiscoveryState
```
DiscoveryState {
    publisher: Mutex<Option<DiscoveryPublisher>>,
    listener_shared_id: Mutex<Option<u64>>,
}
```
Owns: `setup`, `publish`, `feed_region_id`, `listener_shared_id`.

#### Timer: replaced by async
The `timer_tx` field and dedicated timer thread are removed. Sleep hostcalls spawn a tokio task:
```rust
tokio::spawn(async move {
    tokio::time::sleep(duration).await;
    // Mark operation ready, wake guest via mailbox
});
```

## Async Architecture

### Where tokio lives

The binary crate that embeds `selium-runtime` uses `#[tokio::main]`. The runtime crate does not call `tokio::runtime::Builder::build()` — it uses `tokio::spawn` and `tokio::time::sleep` which require being in a tokio context (provided by the binary's `#[tokio::main]`).

Tests in the runtime crate use `#[tokio::test]`.

### Network I/O moved from kernel to runtime

Current (in kernel `network_runtime.rs`):
```rust
impl Kernel {
    pub fn tcp_bind(&self, address: String) -> Result<HostQueueDescriptor> {
        // Creates TcpListener, spawns std::thread for accept loop
    }
    pub fn tcp_connect(&self, address: String) -> Result<SharedRegionDescriptor> {
        // Creates TcpStream, spawns std::thread for proxy_inbound/outbound
    }
    pub fn udp_bind(&self, address: String) -> Result<SharedRegionDescriptor> {
        // Creates UdpSocket, spawns std::thread for recv/send
    }
}
```

After:
```rust
// kernel: only metadata
impl NetworkState {
    pub(crate) fn insert_tcp_listener(&self, local_id: u64, shared_id: u64) { ... }
    pub(crate) fn close_tcp_listener(&self, local_id: u64) -> Result<()> { ... }
    // etc.
}

// runtime/src/network.rs (new)
impl Runtime {
    pub async fn tcp_bind(&self, address: String) -> Result<HostQueueDescriptor> {
        let listener = TcpListener::bind(&address).await?;
        let descriptor = self.kernel.queues().create_host_queue();
        self.kernel.network().insert_tcp_listener(descriptor.local_id, descriptor.shared_id);
        let running = Arc::new(AtomicBool::new(true));
        tokio::spawn(accept_loop(
            listener,
            self.kernel.clone(),
            descriptor.local_id,
            running,
        ));
        Ok(descriptor)
    }
}

async fn accept_loop(listener: TcpListener, kernel: Kernel, queue_local_id: u64, running: Arc<AtomicBool>) {
    while running.load(Ordering::Relaxed) {
        match listener.accept().await {
            Ok((stream, _addr)) => {
                // Create stream region, spawn proxy task
                let (region, inbound_writer, outbound_reader) = create_stream_region(&kernel)?;
                kernel.network().insert_tcp_stream(region.shared_id, ...);
                tokio::spawn(run_tcp_proxy(stream, inbound_writer, outbound_reader, running.clone()));
                kernel.queues().host_queue_send(queue_local_id, 0, region.shared_id)?;
            }
            Err(e) => { /* handle */ }
        }
    }
}
```

The `create_stream_region` helper moves to the runtime crate. It uses `kernel.memory()` to allocate shared regions and `selium_shm` primitives for ring buffer setup — same logic, different crate.

### Timer: before and after

Before:
```rust
// state.rs: spawns dedicated thread with mpsc channel
let (timer_tx, timer_rx) = std::sync::mpsc::channel();
std::thread::spawn(move || {
    for request in timer_rx {
        std::thread::sleep(request.deadline - Instant::now());
        // Mark operation ready, wake via mailbox
    }
});
```

After (in `dispatch_hostcall`):
```rust
HostcallRequest::Sleep { millis } => {
    let duration = Duration::from_millis(millis);
    let engine = self.hostcalls.clone(); // Arc'd internally
    tokio::spawn(async move {
        tokio::time::sleep(duration).await;
        if let Some(op) = engine.operations().lock().get_mut(&operation_id) {
            op.state = HostOperationState::Ready(HostcallOutput::Empty);
        }
        if let Some(mailbox) = engine.mailboxes().lock().get(&process_id) {
            drop(mailbox.enqueue(task_id));
        }
    });
    Ok(HostOperationState::SleepWait { deadline: Instant::now() + duration })
}
```

Note: the `SleepWait` variant still exists because the guest may poll before the timer fires. But the timer thread is gone — replaced by individual tokio tasks. If there are many concurrent sleeps this could mean many tasks, but tokio handles this efficiently (sleep tasks are cheap, just registered in a timer wheel).

### What doesn't become async

- **Guest WASM execution.** `WasmApplication::call_function` is synchronous and remains so. Hostcalls that execute during guest code are dispatched synchronously.
- **`wait_for_activity_from`.** Used during bootstrap, which runs before the async system is fully operational. Blocking on a condvar during startup is fine.
- **Guest `hostcall_poll`.** Guests are cooperatively scheduled — they call poll to check pending operations. The poll path reads state set by async tasks but is itself synchronous.

## API Shape After Refactor

### Kernel public API

```rust
// Accessors
impl Kernel {
    pub fn memory(&self) -> &MemoryRegistry { &self.inner.memory }
    pub fn processes(&self) -> &ProcessTable { &self.inner.processes }
    pub fn storage(&self) -> &StorageRegistry { &self.inner.storage }
    pub fn network(&self) -> &NetworkState { &self.inner.network }
    pub fn queues(&self) -> &HostQueueRegistry { &self.inner.queues }
}

// Or: sub-structs are Clone-able handles themselves, each wrapping Arc<...>
// This avoids the accessor pattern and lets callers hold onto sub-struct refs.
```

Decision: **sub-structs are Clone handles** wrapping `Arc<Inner>`. Each sub-struct's inner type is `pub(crate)` and the handle type is `pub`. This matches the existing `Kernel` pattern. Callers clone sub-struct handles they need.

```rust
let kernel = Kernel::default();
let processes = kernel.processes(); // Returns ProcessTable (Clone)
let memory = kernel.memory();       // Returns MemoryRegistry (Clone)

// Each can be used independently
processes.start_process("mod", "main", grants);
let (shared_id, len) = memory.allocate_shared_region(64)?;
```

The `Kernel` struct itself becomes a thin wrapper — just the `Arc<KernelInner>`. It could even be removed, with `KernelInner`'s fields exposed directly as Clone handles. But keeping it provides a single "entry point" that owns the `id_seed`.

### Runtime public API

Same pattern:
```rust
impl Runtime {
    pub fn kernel(&self) -> Kernel { self.inner.kernel.clone() }
    pub fn guests(&self) -> GuestTable { self.inner.guests.clone() }
    pub fn authorities(&self) -> ProcessAuthorityTable { self.inner.authorities.clone() }
    pub fn resources(&self) -> ResourceTracker { self.inner.resources.clone() }
    pub fn hostcalls(&self) -> HostcallEngine { self.inner.hostcalls.clone() }
    pub fn discovery(&self) -> DiscoveryState { self.inner.discovery.clone() }
}
```

## File Layout After Refactor

### kernel/src/
```
state.rs          — Kernel, KernelInner, shared types (ProcessState, etc.)
memory.rs          — MemoryRegistry + impl
process.rs         — ProcessTable + impl
storage.rs         — StorageRegistry + impl
network.rs         — NetworkState + impl, metadata types
host_queue.rs      — HostQueueRegistry + impl
backend.rs         — KernelBackend (uses &MemoryRegistry)
error.rs           — (unchanged)
lib.rs             — pub use all sub-structs
```
Removed: `network_runtime.rs` (moves to runtime).

### runtime/src/
```
state.rs           — Runtime, sub-struct types
bootstrap.rs       — bootstrap_system_guests, spawn_system_guest (uses &Runtime)
hostcall.rs        — dispatch_hostcall function, hostcall engine methods
network.rs         — NEW: tcp_bind, tcp_connect, udp_bind, proxy tasks
host_functions.rs  — wasm host function registration (unchanged pattern)
process.rs         — stop_process, authorises, cleanup functions
discovery.rs       — DiscoverState impl, registration_uris (unchanged)
wasm.rs            — (unchanged)
mailbox.rs         — (unchanged)
error.rs           — (unchanged)
config.rs          — (unchanged)
lib.rs             — pub use all sub-structs
```
