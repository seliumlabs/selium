## MODIFIED Requirements

### Requirement: Discovery request variants
`DiscoveryRequest` SHALL include the following variants:
- `Resolve(String)` — resolve a URI to a resource target
- `Register { uri: String, target: ResourceTarget }` — register a URI→target mapping
- `Revoke { uri: String }` — remove a URI→target mapping

The `Register` and `Revoke` variants MAY be used by both the runtime (Tier 1, authoritative) and guests (Tier 2, validated). The discovery guest SHALL distinguish callers by `client_process_id` on the RPC connection.

#### Scenario: Register variant serialises round-trip
- **WHEN** a `DiscoveryRequest::Register { uri: "sel://x", target }` is serialised via rkyv and deserialised
- **THEN** the deserialised value SHALL equal the original

#### Scenario: Revoke variant serialises round-trip
- **WHEN** a `DiscoveryRequest::Revoke { uri: "sel://x" }` is serialised via rkyv and deserialised
- **THEN** the deserialised value SHALL equal the original

### Requirement: Discovery response variants
`DiscoveryResponse` SHALL include the following variants:
- `Found(ResourceTarget)` — the requested URI was found
- `NotFound` — the requested URI was not found
- `Registered` — the URI was successfully registered
- `Revoked` — the URI was successfully revoked
- `Forbidden` — the caller is not authorised to register the given target (Tier 2 validation failure)

#### Scenario: Registered variant serialises round-trip
- **WHEN** `DiscoveryResponse::Registered` is serialised via rkyv and deserialised
- **THEN** the deserialised value SHALL equal `DiscoveryResponse::Registered`

#### Scenario: Forbidden variant serialises round-trip
- **WHEN** `DiscoveryResponse::Forbidden` is serialised via rkyv and deserialised
- **THEN** the deserialised value SHALL equal `DiscoveryResponse::Forbidden`

### Requirement: AllocRegion hostcall with purpose
`HostcallRequest::AllocRegion` SHALL include a non-optional `purpose: ResourceKind` field. The runtime SHALL record the purpose alongside the allocation for discovery registration but SHALL NOT use it for access control decisions.

#### Scenario: AllocRegion with LogChannel purpose
- **WHEN** a guest invokes `AllocRegion { pages: 16, prot: ReadWrite, purpose: ResourceKind::LogChannel }`
- **THEN** the runtime SHALL allocate the region and record its purpose as `LogChannel`

## ADDED Requirements

### Requirement: ResourceKind enum
`selium-abi` SHALL define a `ResourceKind` enum with the following variants:
- `LogChannel` — shared memory region for tracing log transport
- `LiveTable` — shared memory region for a live table
- `RpcRing` — shared memory region for RPC request/reply rings
- `PubSubTopic` — shared memory region for pub/sub topic
- `NetworkBuffer` — shared memory region for network socket buffers
- `DurableLog` — shared memory region for durable log storage
- `BlobStore` — shared memory region for blob store
- `SharedMemory` — generic/unknown shared memory region

The enum SHALL NOT be used for AAA (authentication, authorisation, or audit) decisions. A malicious guest MAY spoof the value; the only effect is cosmetic (misleading UI or discovery entry).

#### Scenario: ResourceKind serialises round-trip
- **WHEN** `ResourceKind::LogChannel` is serialised via rkyv and deserialised
- **THEN** the deserialised value SHALL equal `ResourceKind::LogChannel`

### Requirement: Channel backpressure enum
`selium-abi` SHALL define a `ChannelBackpressure` enum with variants:
- `Park` — writers respect blocking reader positions (default); writes block when consumers fall behind
- `Drop` — writers never block; slow consumers may lose data

#### Scenario: ChannelBackpressure serialises round-trip
- **WHEN** `ChannelBackpressure::Drop` is serialised and deserialised
- **THEN** the value SHALL be preserved

### Requirement: Guest log register hostcall
`HostcallRequest` SHALL include a `GuestLogRegister { shared_id: SharedResourceId }` variant. The runtime SHALL validate that `shared_id` was allocated by the calling process and SHALL attach to the shared region as a non-blocking log reader, making log entries available via the existing `GuestLogRead` hostcall.

#### Scenario: Guest registers log channel with kernel
- **WHEN** a guest sends `HostcallRequest::GuestLogRegister { shared_id: 42 }`
- **THEN** the kernel SHALL attach to shared region 42 as a non-blocking log reader
- **AND** the hostcall SHALL complete with `HostcallOutput::Empty`

#### Scenario: GuestLogRegister rejected for foreign shared_id
- **WHEN** a guest sends `HostcallRequest::GuestLogRegister { shared_id }` where `shared_id` was allocated by a different process
- **THEN** the runtime SHALL return an error and SHALL NOT attach to the region
