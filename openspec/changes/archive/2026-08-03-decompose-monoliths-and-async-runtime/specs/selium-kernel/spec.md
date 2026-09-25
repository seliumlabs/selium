## MODIFIED Requirements

### Requirement: Shared Memory Regions (modified)
`selium-kernel` SHALL expose a `MemoryRegistry` sub-struct that manages shared memory regions as first-class primitive resources. Regions can be allocated, attached, detached, and accessed independently of a guest's private linear memory.

#### Scenario: Shared region attached to two guests
- **WHEN** two guests attach the same valid shared memory region
- **THEN** both guests SHALL be able to access the region according to the runtime memory model

### Requirement: Protocol-Neutral Network Primitives (modified)
`selium-kernel` SHALL expose a `NetworkState` sub-struct that tracks metadata for network resources (listeners, streams, sockets). The kernel SHALL NOT spawn I/O threads, perform socket I/O, or own live OS socket handles. Network I/O is the responsibility of `selium-runtime`.

#### Scenario: Kernel tracks listener metadata
- **WHEN** the runtime creates a TCP listener
- **THEN** the kernel's `NetworkState` SHALL store the listener metadata (`shared_id`, `running` flag) for lifecycle management

#### Scenario: Kernel does not spawn threads
- **WHEN** any network operation is invoked on the kernel
- **THEN** no `std::thread::spawn` call occurs in `selium-kernel`

### Requirement: Durable Storage Primitives (unchanged)
`selium-kernel` SHALL expose a `StorageRegistry` sub-struct with durable log and blob primitives: append, replay, checkpoint, put, and get operations.

#### Scenario: Guest replays a durable log
- **WHEN** a guest replays a durable log from a valid checkpoint or sequence
- **THEN** the kernel SHALL return the retained records and bounds according to the storage contract

### Requirement: Primitive Process Lifecycle (modified)
`selium-kernel` SHALL expose a `ProcessTable` sub-struct for starting, stopping, and inspecting guest processes without embedding placement or orchestration policy.

#### Scenario: Runtime starts configured guest process
- **WHEN** the runtime requests a new guest process using a valid module and entrypoint
- **THEN** the kernel SHALL create the process resource and return an inspectable process identity

### Requirement: Activity and Metering Hooks (unchanged)
`selium-kernel` SHALL expose hooks in `ProcessTable` that allow the runtime to project lifecycle events and resource-usage observations into host-visible logs and metering streams.

#### Scenario: Guest process consumes resources
- **WHEN** a guest process uses CPU, memory, storage, or bandwidth
- **THEN** the kernel SHALL make those observations available to the runtime through the metering hooks

### Requirement: Kernel Consumes the Shared Ring Implementation (unchanged)
The kernel SHALL use the shared ring protocol implementation for network proxies and guest log drains. Bespoke frame codecs, reservation logic, slot scans, and multi-memory header handling SHALL NOT exist in the kernel.

#### Scenario: Log drain uses shared frame reader
- **WHEN** the kernel drains a guest log channel
- **THEN** it reads frames with the shared frame reader and ring geometry from the channel header, with no local frame parsing

### Requirement: Host Queue Primitives (modified)
`selium-kernel` SHALL expose a `HostQueueRegistry` sub-struct for host-mediated connection queues. Queues support create, attach, send, and non-blocking receive. The kernel SHALL NOT provide async receive — polling is the runtime's responsibility.

#### Scenario: Runtime sends value to queue
- **WHEN** the runtime sends a value to a host queue via `HostQueueRegistry::host_queue_send`
- **THEN** the value SHALL be available to `try_host_queue_recv` on the receiving end

#### Scenario: Sync-only receive
- **WHEN** `selium-kernel` is compiled
- **THEN** it SHALL NOT contain any async functions

## ADDED Requirements

### Requirement: Sub-Struct Architecture
`selium-kernel` SHALL decompose `Kernel` into five public sub-structs, each with its own methods and no cross-subsystem field access:

- `MemoryRegistry` — shared memory regions, mappings, Store
- `ProcessTable` — process lifecycle, activity log, guest logs, metering
- `StorageRegistry` — durable logs and blob stores
- `NetworkState` — network resource metadata (no I/O)
- `HostQueueRegistry` — host-mediated connection queues

#### Scenario: Sub-structs are independently usable
- **WHEN** a caller holds a `ProcessTable` handle
- **THEN** the caller SHALL be able to call `start_process`, `stop_process`, and other process methods without accessing `Kernel` directly

#### Scenario: No cross-subsystem field access
- **WHEN** `MemoryRegistry` methods execute
- **THEN** they SHALL NOT lock or access fields owned by `ProcessTable`, `StorageRegistry`, `NetworkState`, or `HostQueueRegistry`

### Requirement: No Tokio Dependency
`selium-kernel` SHALL NOT depend on tokio. The `HostQueueState` notification mechanism SHALL use `parking_lot` primitives only.

#### Scenario: Kernel compiles without tokio
- **WHEN** `selium-kernel` is built
- **THEN** tokio SHALL NOT appear in its dependency tree

## REMOVED Requirements

### Requirement: Network I/O in Kernel (removed)
The kernel no longer spawns network I/O threads. The following are removed:

- ~~`Kernel::tcp_bind` spawning accept loop~~
- ~~`Kernel::tcp_connect` spawning proxy threads~~
- ~~`Kernel::udp_bind` spawning recv/send threads~~
- ~~`proxy_inbound`, `proxy_outbound`, `run_proxy`, `run_udp_proxy`~~
- ~~`tcp_accept_loop`, `udp_proxy_recv`, `udp_proxy_send`~~

### Requirement: Async Host Queue Receive (removed)
The async `host_queue_recv` method is dead code and SHALL be removed.
