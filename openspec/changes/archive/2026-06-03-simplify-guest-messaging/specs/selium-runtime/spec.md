## MODIFIED Requirements

### Requirement: Wasmtiny-Backed Guest Execution
`selium-runtime` SHALL execute Selium guests using Wasmtiny as the WebAssembly runtime substrate, including the mmap-backed shared memory primitives.

#### Scenario: Runtime starts a guest module
- **WHEN** the runtime starts a valid guest module
- **THEN** it SHALL instantiate and execute that guest through Wasmtiny with access to `alloc_region`, `free_region`, and `attach_region` host functions

## ADDED Requirements

### Requirement: Shared Memory Hostcall Passthrough
`selium-runtime` SHALL dispatch `AllocRegion`, `FreeRegion`, and `AttachRegion` hostcalls directly to wasmtiny's shared memory registry without additional kernel-layer mediation beyond capability validation.

#### Scenario: Authorised region allocation
- **WHEN** a guest with a capability grant for shared memory invokes `AllocRegion`
- **THEN** the runtime SHALL validate the capability and delegate the allocation to wasmtiny

#### Scenario: Unauthorised region allocation denied
- **WHEN** a guest without a shared memory capability invokes `AllocRegion`
- **THEN** the runtime SHALL return `AbiErrorCode::CapabilityDenied`

### Requirement: Region Lifetime Tied to Guest Lifecycle
When a guest instance terminates, `selium-runtime` SHALL automatically call `free_region` on all regions allocated by or attached to that guest.

#### Scenario: Guest exits with attached regions
- **WHEN** a guest that has attached to shared regions exits
- **THEN** the runtime SHALL unmap all shared regions from that guest's memory before releasing the instance

## REMOVED Requirements

### Requirement: HostQueue Hostcall Dispatch
**Reason**: `HostQueueSend` and `HostQueueRecv` are removed from the ABI alongside the RPC extraction to `selium-rpc`.
**Migration**: RPC connection handoff is handled by `selium-rpc` using the new shared memory ABI directly.
