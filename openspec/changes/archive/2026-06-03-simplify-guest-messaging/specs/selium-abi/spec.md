## ADDED Requirements

### Requirement: Shared Region Hostcall Variants
`selium-abi` SHALL define `AllocRegion`, `FreeRegion`, and `AttachRegion` variants on `HostcallRequest` with the following payloads:

- `AllocRegion { pages: u32, prot: RegionProt }` returning `(region_id: u64, page_offset: u32)`
- `FreeRegion { region_id: u64 }` returning unit
- `AttachRegion { region_id: u64, reader_slot: Option<u32>, prot: RegionProt }` returning `page_offset: u32`

#### Scenario: AllocRegion hostcall round-trip
- **WHEN** a guest encodes `HostcallRequest::AllocRegion { pages: 16, prot: ReadWrite }` and the runtime processes it
- **THEN** the hostcall SHALL complete with `HostcallOutput::AllocRegion { region_id, page_offset }` where `page_offset` is the base page within guest linear memory

#### Scenario: AttachRegion with reader slot
- **WHEN** a guest encodes `HostcallRequest::AttachRegion { region_id: 7, reader_slot: Some(3), prot: ReadOnly }` and the runtime processes it
- **THEN** the hostcall SHALL complete with `HostcallOutput::AttachRegion { page_offset }` and only page 3 of the mapped range SHALL be writable

### Requirement: RegionProt Enum
`selium-abi` SHALL define a `RegionProt` enum with variants `ReadOnly` and `ReadWrite`.

#### Scenario: RegionProt serialization
- **WHEN** a `RegionProt::ReadOnly` value is encoded in a hostcall payload
- **THEN** it SHALL be represented as `0u8` and `RegionProt::ReadWrite` as `1u8`

## REMOVED Requirements

### Requirement: Stable Hostcall Contracts (Signal and SharedMemory variants)
**Reason**: `Signal` and `SharedMemory` hostcalls are replaced by `alloc_region`/`free_region`/`attach_region` plus native WASM atomic instructions (`memory.atomic.wait32`/`notify`).
**Migration**: Existing guest code using `Signal::wait` must switch to `memory.atomic.wait32` on the shared region's generation counter. Existing guest code using `SharedMemory` read/write hostcalls must switch to direct load/store at the page offset returned by `alloc_region`/`attach_region`.

### Requirement: Host-Mediated Connection Queue Hostcalls (MODIFIED)
**Status**: `HostQueueCreate`, `HostQueueAttach`, `HostQueueSend`, and `HostQueueRecv` remain in the core ABI.
**Reason**: While RPC connection handoff has moved to `selium-rpc` using the shared memory ABI directly, host queues remain necessary for the TcpListener accept mechanism. When a TcpListener accepts a connection, the kernel enqueues the incoming connection info into a host queue, and the guest retrieves it via `HostQueueRecv`.
**Migration**: RPC-specific usage of host queues has been removed; host queues now serve only the TCP listener accept pattern.

### Requirement: UdpBind Hostcall (RETAINED AS STUB)
**Status**: `UdpBind` remains in the ABI but is implemented as a stub that returns errors.
**Reason**: UDP socket creation will return region IDs that the guest attaches directly in a follow-up networking change. The current implementation maintains ABI compatibility while the networking module is updated.
**Migration**: UDP functionality is temporarily gated behind the old ABI compatibility layer until the networking module is updated.
