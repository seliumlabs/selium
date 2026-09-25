## MODIFIED Requirements

### Requirement: Wasmtiny-Backed Guest Execution
`selium-runtime` SHALL execute Selium guests using Wasmtiny as the WebAssembly runtime substrate, including the mmap-backed shared memory primitives and hostcall dispatch for networking and RPC.

#### Scenario: Runtime starts a guest module
- **WHEN** the runtime starts a valid guest module
- **THEN** it SHALL instantiate and execute that guest through Wasmtiny with access to `alloc_region`, `free_region`, `attach_region`, `TcpConnect`, `TcpBind`, and `UdpBind` host functions

### Requirement: Shared Memory Hostcall Passthrough
`selium-runtime` SHALL dispatch `AllocRegion`, `FreeRegion`, and `AttachRegion` hostcalls directly to wasmtiny's shared memory registry without additional kernel-layer mediation beyond capability validation.

#### Scenario: Authorised region allocation
- **WHEN** a guest with a capability grant for shared memory invokes `AllocRegion`
- **THEN** the runtime SHALL validate the capability and delegate the allocation to wasmtiny

#### Scenario: Unauthorised region allocation denied
- **WHEN** a guest without a shared memory capability invokes `AllocRegion`
- **THEN** the runtime SHALL return `AbiErrorCode::PermissionDenied`

### Requirement: Region Lifetime Tied to Guest Lifecycle
When a guest instance terminates, `selium-runtime` SHALL automatically call `free_region` on all regions allocated by or attached to that guest.

#### Scenario: Guest exits with attached regions
- **WHEN** a guest that has attached to shared regions exits
- **THEN** the runtime SHALL unmap all shared regions from that guest's memory before releasing the instance

## ADDED Requirements

### Requirement: Network Hostcall Dispatch
`selium-runtime` SHALL dispatch `TcpConnect`, `TcpBind`, and `UdpBind` hostcalls to the kernel, which allocates shared regions with the standard ring buffer coordination layout and spawns proxy threads.

#### Scenario: Guest connects to TCP endpoint via runtime
- **WHEN** a guest with a `Network` capability invokes `TcpConnect`
- **THEN** the runtime SHALL validate the capability and delegate to the kernel, returning a `SharedRegionDescriptor` with the standard layout

#### Scenario: Guest binds UDP socket via runtime
- **WHEN** a guest with a `Network` capability invokes `UdpBind`
- **THEN** the runtime SHALL validate the capability and delegate to the kernel, returning a `SharedRegionDescriptor` with the standard layout

### Requirement: Discovery Bootstrap Integration
`selium-runtime` SHALL pass the discovery host queue's `shared_id` to application guest entrypoints as the `discovery_handle` argument, enabling guests to construct a `Context` for URI resolution.

#### Scenario: Application guest receives discovery handle
- **WHEN** the runtime bootstraps an application guest
- **THEN** the guest's entrypoint SHALL receive the discovery `shared_id` as a u64 argument, which it passes to `Context::from_raw`

## REMOVED Requirements

### Requirement: HostQueue Hostcall Dispatch
**Reason**: This removal was incorrectly specified in the previous migration. `HostQueue` hostcalls are retained — they are required for TCP listener accept and RPC connection handshake patterns.
**Migration**: No migration needed. `HostQueueCreate`, `HostQueueAttach`, `HostQueueSend`, and `HostQueueRecv` remain dispatched by the runtime and are not removed.
