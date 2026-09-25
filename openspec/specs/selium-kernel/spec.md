## Purpose

Expose Selium's low-level host primitives for shared memory, network, storage, process lifecycle, activity, and metering.

## Requirements

### Requirement: Shared Memory Regions
`selium-kernel` SHALL expose a `MemoryRegistry` sub-struct that manages shared memory regions as first-class primitive resources. Regions can be allocated, attached, detached, and accessed independently of a guest's private linear memory.

#### Scenario: Shared region attached to two guests
- **WHEN** two guests attach the same valid shared memory region
- **THEN** both guests SHALL be able to access the region according to the runtime memory model

### Requirement: Protocol-Neutral Network Primitives
`selium-kernel` SHALL expose a `NetworkState` sub-struct that tracks metadata for network resources (listeners, streams, sockets). The kernel SHALL NOT spawn I/O threads, perform socket I/O, or own live OS socket handles. Network I/O is the responsibility of `selium-runtime`.

#### Scenario: Kernel tracks listener metadata
- **WHEN** the runtime creates a TCP listener
- **THEN** the kernel's `NetworkState` SHALL store the listener metadata (`shared_id`, `running` flag) for lifecycle management

#### Scenario: Kernel does not spawn threads
- **WHEN** any network operation is invoked on the kernel
- **THEN** no `std::thread::spawn` call occurs in `selium-kernel`

### Requirement: Kernel Consumes the Shared Ring Implementation

The kernel SHALL use the shared ring protocol implementation for network
proxies and guest log drains. Bespoke frame codecs, reservation logic,
slot scans, and multi-memory header handling SHALL NOT exist in the
kernel.

#### Scenario: Network proxy uses shared primitives

- **WHEN** the kernel proxies a TCP/UDP stream to or from a guest ring
- **THEN** frame reads/writes, reservations, and reader-slot updates go
  through the shared ring primitives, not kernel-local copies

#### Scenario: Log drain uses shared frame reader

- **WHEN** the kernel drains a guest log channel
- **THEN** it reads frames with the shared frame reader and ring geometry
  from the channel header, with no local frame parsing

### Requirement: Durable Storage Primitives
`selium-kernel` SHALL expose a `StorageRegistry` sub-struct with durable log and blob primitives: append, replay, checkpoint, put, and get operations.

#### Scenario: Guest replays a durable log
- **WHEN** a guest replays a durable log from a valid checkpoint or sequence
- **THEN** the kernel SHALL return the retained records and bounds according to the storage contract

### Requirement: Primitive Process Lifecycle
`selium-kernel` SHALL expose a `ProcessTable` sub-struct for starting, stopping, and inspecting guest processes without embedding placement or orchestration policy.

#### Scenario: Runtime starts configured guest process
- **WHEN** the runtime requests a new guest process using a valid module and entrypoint
- **THEN** the kernel SHALL create the process resource and return an inspectable process identity

### Requirement: Activity and Metering Hooks
`selium-kernel` SHALL expose hooks in `ProcessTable` that allow the runtime to project lifecycle events and resource-usage observations into host-visible logs and metering streams.

#### Scenario: Guest process consumes resources
- **WHEN** a guest process uses CPU, memory, storage, or bandwidth
- **THEN** the kernel SHALL make those observations available to the runtime through the metering hooks

### Requirement: Host Queue Primitives
`selium-kernel` SHALL expose a `HostQueueRegistry` sub-struct for host-mediated connection queues. Queues support create, attach, send, and non-blocking receive. The kernel SHALL NOT provide async receive — polling is the runtime's responsibility.

#### Scenario: Runtime sends value to queue
- **WHEN** the runtime sends a value to a host queue via `HostQueueRegistry::host_queue_send`
- **THEN** the value SHALL be available to `try_host_queue_recv` on the receiving end

#### Scenario: Sync-only receive
- **WHEN** `selium-kernel` is compiled
- **THEN** it SHALL NOT contain any async functions

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

### Requirement: Shared Region Layout Header
`selium-kernel` shared memory regions SHALL support a layout header (magic, capacity, memory count, per-memory offset/length pairs) so that multiple parties can discover sub-memories after attaching via `shared_id`. Each sub-memory SHALL use the standard ring buffer coordination layout with generation counter, `next_tail`, `writer_count`, and `reader_slots` in page 0.

#### Scenario: Two guests attach the same region and agree on layout
- **WHEN** a guest seals a region built with `SharedRegionBuilder` and another guest attaches the same `shared_id`
- **THEN** both parties SHALL read the identical layout header and enumerate the same sub-memories, each with the standard coordination fields

### Requirement: Per-Connection RPC Session Isolation
`selium-kernel` SHALL enforce that a `SharedRegion` allocated for an RPC session is only accessible to the two authorised parties. No other guest SHALL be able to attach or read that region without possessing its `shared_id`.

#### Scenario: Unauthorised guest attempts to attach a session region
- **WHEN** a guest without the `shared_id` tries to attach a session region
- **THEN** the kernel SHALL deny the attachment

### Requirement: Event-Driven Network Poller
The kernel SHALL provide a single OS-event-port poller thread (epoll/
kqueue/IOCP via mio) that owns all network proxy socket readiness.
Proxy sockets — TCP streams for inbound reads, TCP listeners for
accepts, and UDP sockets for receives — SHALL be registered by shared
region id token. A readable event SHALL pump the available bytes or
datagrams into the corresponding inbound ring, advance the ring's
generation, and invoke the runtime's generation-advance callback so
registered guest tasks are woken via the mailbox.

#### Scenario: Socket data reaches the inbound ring without polling
- **WHEN** a proxy socket becomes readable while no guest is executing
- **THEN** the poller thread SHALL deliver the data to the inbound ring
  and bridge to the guest wake path without any sleep-based retry loop

#### Scenario: Accept is event-driven
- **WHEN** a registered listener becomes readable
- **THEN** the poller SHALL accept the connection, create its stream
  region, and enqueue it on the host queue from within the poller

### Requirement: Poller Registration Hygiene
The kernel poller SHALL deregister a socket and release its entry when
the socket reaches EOF, fails with a fatal error, or its running flag
is cleared. Accept callbacks and generation-advance callbacks SHALL run
without holding poller registry locks, so that callbacks may register
new sockets (including re-entrantly from a guest reactor executed on
the poller thread).

#### Scenario: Closed connections do not leak registrations
- **WHEN** a proxied stream observes EOF or a fatal read error
- **THEN** its fd SHALL be removed from the poller registry and its
  entry SHALL be dropped

#### Scenario: Callbacks may register new sockets
- **WHEN** an accept callback or generation-advance callback runs code
  that registers another socket with the same poller
- **THEN** registration SHALL complete without deadlock
