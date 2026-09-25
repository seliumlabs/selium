## MODIFIED Requirements

### Requirement: Network Hostcall Dispatch (modified)
`selium-runtime` SHALL dispatch `TcpConnect`, `TcpBind`, and `UdpBind` hostcalls by spawning tokio tasks that manage the underlying OS sockets and proxy data through shared ring buffers. The kernel's `NetworkState` SHALL be used only for metadata tracking.

#### Scenario: Guest connects to TCP endpoint via runtime
- **WHEN** a guest with a `Network` capability invokes `TcpConnect`
- **THEN** the runtime SHALL validate the capability, create a shared ring region via the kernel's `MemoryRegistry`, record metadata in the kernel's `NetworkState`, spawn a tokio task for the TCP proxy, and return a `SharedRegionDescriptor`

#### Scenario: Guest binds TCP listener via runtime
- **WHEN** a guest with a `Network` capability invokes `TcpBind`
- **THEN** the runtime SHALL validate the capability, bind an async `TcpListener`, create a host queue via `HostQueueRegistry`, record metadata in `NetworkState`, spawn a tokio task for the accept loop, and return a `HostQueueDescriptor`

#### Scenario: Guest binds UDP socket via runtime
- **WHEN** a guest with a `Network` capability invokes `UdpBind`
- **THEN** the runtime SHALL validate the capability, bind an async `UdpSocket`, create a shared ring region via `MemoryRegistry`, record metadata in `NetworkState`, spawn tokio tasks for recv/send proxying, and return a `SharedRegionDescriptor`

### Requirement: Runtime Uses Tokio for Timers (modified)
`selium-runtime` SHALL replace the dedicated timer driver thread with `tokio::spawn` tasks using `tokio::time::sleep`. The `Sleep` hostcall SHALL spawn a tokio task that marks the operation ready and wakes the guest mailbox when the deadline arrives.

#### Scenario: Guest calls Sleep
- **WHEN** a guest invokes `Sleep { millis: 100 }`
- **THEN** the runtime SHALL spawn a tokio task that sleeps for 100ms, then marks the operation ready and enqueues the task ID in the guest's mailbox

#### Scenario: No dedicated timer thread
- **WHEN** the runtime is constructed
- **THEN** no `std::thread::spawn` for timer management occurs
- **AND** no `std::sync::mpsc::channel` for timer requests is created

### Requirement: Wasmtiny-Backed Guest Execution (unchanged)
`selium-runtime` SHALL execute Selium guests using Wasmtiny as the WebAssembly runtime substrate, including the mmap-backed shared memory primitives and hostcall dispatch for networking and RPC.

#### Scenario: Runtime starts a guest module
- **WHEN** the runtime starts a valid guest module
- **THEN** it SHALL instantiate and execute that guest through Wasmtiny with access to `alloc_region`, `free_region`, `attach_region`, `TcpConnect`, `TcpBind`, and `UdpBind` host functions

### Requirement: Shared Memory Hostcall Passthrough (unchanged)
`selium-runtime` SHALL dispatch `AllocRegion`, `FreeRegion`, and `AttachRegion` hostcalls to the kernel's `MemoryRegistry` without additional kernel-layer mediation beyond capability validation.

#### Scenario: Authorised region allocation
- **WHEN** a guest with a capability grant for shared memory invokes `AllocRegion`
- **THEN** the runtime SHALL validate the capability and delegate the allocation to `MemoryRegistry`

#### Scenario: Unauthorised region allocation denied
- **WHEN** a guest without a shared memory capability invokes `AllocRegion`
- **THEN** the runtime SHALL return `AbiErrorCode::PermissionDenied`

### Requirement: Region Lifetime Tied to Guest Lifecycle (unchanged)
When a guest instance terminates, `selium-runtime` SHALL automatically clean up all regions allocated by or attached to that guest.

#### Scenario: Guest exits with attached regions
- **WHEN** a guest that has attached to shared regions exits
- **THEN** the runtime SHALL unmap all shared regions from that guest's memory before releasing the instance

### Requirement: Discovery-Enabled Bootstrap (unchanged)
`selium-runtime` SHALL support discovery-enabled bootstrap with tagged entrypoint arguments and per-guest readiness gating.

#### Scenario: Discovery wiring uses tagged argument encoding
- **WHEN** the runtime injects discovery arguments into a guest descriptor
- **THEN** `decode_wasm_arguments` decodes every injected value without error, for all possible u64 handle values

#### Scenario: Readiness is per-guest
- **WHEN** a bootstrapped guest does not call `mark_ready()` within the readiness window
- **THEN** the runtime rolls back the bootstrap and reports `ReadinessUnsatisfied` naming that guest

### Requirement: Grant Admission and Evaluation (unchanged)
`selium-runtime` SHALL reject, at spawn or `ProcessStart`, any grant whose selectors it cannot evaluate, and SHALL evaluate every accepted grant against authority-derived scope contexts.

#### Scenario: Accept-then-deny is impossible
- **WHEN** a guest is spawned with a grant the runtime would never be able to satisfy (unevaluatable selector)
- **THEN** spawning fails immediately with the selector named

#### Scenario: Errors attribute correctly
- **WHEN** any authorisation check fails
- **THEN** the error identifies the denied capability and the relevant scope values (tenant/class/identity)

## ADDED Requirements

### Requirement: Sub-Struct Architecture
`selium-runtime` SHALL decompose `Runtime` into sub-structs, each with its own methods:

- `GuestTable` — loaded guests, module registry
- `ProcessAuthorityTable` — process authorities, grants, parent relationships
- `ResourceTracker` — local handle owners, shared resource owners, region purposes
- `HostcallEngine` — operation state machine, mailboxes
- `DiscoveryState` — discovery publisher, listener handle

#### Scenario: Sub-structs are independently usable
- **WHEN** a caller holds a `ResourceTracker` handle
- **THEN** the caller SHALL be able to call `claim_local_handle`, `release_local_handle`, and other resource tracking methods without accessing `Runtime` directly

#### Scenario: Cross-subsystem orchestration uses function parameters
- **WHEN** an operation requires multiple sub-structs (e.g., `cleanup_process_resources` needs `ResourceTracker`, `DiscoveryState`, and `Kernel`)
- **THEN** it SHALL be implemented as a function that takes the required sub-structs as parameters, rather than as a method on a single monolithic struct

### Requirement: Tokio Dependency
`selium-runtime` SHALL depend on tokio with `net`, `rt-multi-thread`, `sync`, and `time` features. The runtime SHALL NOT spawn its own tokio runtime — it SHALL use `tokio::spawn` and `tokio::time::sleep` from the ambient context provided by the binary's `#[tokio::main]`.

#### Scenario: Runtime uses ambient tokio context
- **WHEN** `Runtime::new` is called outside of a tokio context and a method that spawns tasks is invoked
- **THEN** the call SHALL panic with "there is no reactor running"

#### Scenario: Tests use #[tokio::test]
- **WHEN** a test in `selium-runtime` spawns network resources
- **THEN** the test function SHALL be annotated with `#[tokio::test]`

### Requirement: Async Network I/O
`selium-runtime` SHALL implement TCP and UDP proxy I/O using tokio's async `TcpListener`, `TcpStream`, and `UdpSocket` types. All proxy operations (`proxy_inbound`, `proxy_outbound`, `accept_loop`, `udp_proxy_recv`, `udp_proxy_send`) SHALL be async functions spawned as tokio tasks.

#### Scenario: TCP accept loop uses async I/O
- **WHEN** the runtime's TCP accept loop is running
- **THEN** it SHALL use `listener.accept().await` rather than non-blocking accept with `thread::sleep`

#### Scenario: TCP proxy uses async I/O
- **WHEN** the runtime proxies data between a TCP socket and a shared ring buffer
- **THEN** it SHALL use `stream.read().await` and `stream.write_all().await` rather than blocking `read()`/`write()` with `thread::sleep`
