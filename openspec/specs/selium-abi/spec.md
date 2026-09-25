## Purpose

`selium-abi` defines the core ABI types shared between Selium guest code and the runtime, including hostcall request/response variants, shared region descriptors, discovery protocol types, and resource kind enumerations used throughout the Selium system.

## Requirements

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

### Requirement: Sleep Hostcall Variant
`HostcallRequest` SHALL include a `Sleep { millis: u64 }` variant. The runtime SHALL compute `deadline = Instant::now() + Duration::from_millis(millis)` and store the operation in `HostOperationState::SleepWait { deadline }`. When polled, the runtime SHALL return `CompletionState::Ready(HostcallOutput::Empty)` if `Instant::now() >= deadline`, or `CompletionState::Pending` otherwise.

#### Scenario: Sleep operation created
- **WHEN** the runtime dispatches `HostcallRequest::Sleep { millis: 500 }`
- **THEN** the operation SHALL be stored with a `SleepWait` state whose `deadline` is at least 500ms after the current `Instant::now()`

#### Scenario: Sleep operation polled before deadline
- **WHEN** a `SleepWait` operation is polled and `Instant::now() < deadline`
- **THEN** the poll SHALL return `CompletionState::Pending { operation_id }`

#### Scenario: Sleep operation polled after deadline
- **WHEN** a `SleepWait` operation is polled and `Instant::now() >= deadline`
- **THEN** the poll SHALL return `CompletionState::Ready(HostcallOutput::Empty)`

### Requirement: UdpBind Hostcall
`UdpBind` SHALL return a `SharedRegionDescriptor` containing a multi-memory region with two ring buffers (recv and send), initialised with the standard coordination layout.

#### Scenario: Guest binds UDP socket
- **WHEN** a guest invokes `UdpBind` with a valid address
- **THEN** the host SHALL bind a UDP socket, allocate a shared region with two ring buffers using the standard layout, spawn proxy threads, and return the region descriptor

### Requirement: TcpConnect Hostcall
`TcpConnect` SHALL return a `SharedRegionDescriptor` containing a multi-memory region with two ring buffers (inbound and outbound), initialised with the standard coordination layout.

#### Scenario: Guest connects to TCP endpoint
- **WHEN** a guest invokes `TcpConnect` with a valid address
- **THEN** the host SHALL create a TCP connection, allocate a shared region with two ring buffers using the standard layout, spawn proxy threads, and return the region descriptor

### Requirement: TcpBind Hostcall
`TcpBind` SHALL return a `HostQueueDescriptor` as before, with the kernel spawning an accept loop that creates per-connection shared regions using the standard ring buffer layout.

#### Scenario: Guest binds TCP listener
- **WHEN** a guest invokes `TcpBind` with a valid address
- **THEN** the host SHALL bind a TCP listener, create a host queue, spawn an accept loop, and return the queue descriptor

### Requirement: WaitRegister Hostcall
The ABI SHALL define `HostcallRequest::WaitRegister { region_id,
generation }`, rkyv-encoded like all hostcall requests. The request
registers the calling process's interest in a generation advance of the
identified shared region; the guest task to wake is carried by the
envelope's existing `task_id` field.

#### Scenario: Round-trip encoding
- **WHEN** a `WaitRegister` request is encoded and decoded
- **THEN** `region_id` and `generation` SHALL survive unchanged

#### Scenario: Wake routed via envelope task
- **WHEN** the runtime observes a host-side generation advance past a
  registered generation for that region
- **THEN** it SHALL wake the task identified by the registering
  envelope's `task_id`, and SHALL NOT wake tasks of any other process

#### Scenario: Unattached region rejected
- **WHEN** a process issues `WaitRegister` for a region it has not
  attached
- **THEN** the hostcall SHALL fail loudly

### Requirement: HostQueueCreate Serving Tenant
`HostcallRequest::HostQueueCreate` SHALL carry an optional serving tenant, mirroring `AllocRegion`'s principal-provenance field, and SHALL round-trip through the rkyv codec unchanged.

#### Scenario: Serving tenant round-trips
- **WHEN** a `HostQueueCreate` request with `serving_tenant: Some("acme")` is encoded and decoded
- **THEN** the decoded request's serving tenant SHALL equal the original

### Requirement: Host-Guest Boundary Only

`selium-abi` SHALL define only the host↔guest hostcall contract — the hostcall request, envelope, output, and completion types, and the resource descriptors — plus the shared capability and resource vocabulary those hostcalls carry (`Capability`, `CapabilityGrant`, `ResourceSelector`, `ResourceIdentity`, `LocalityScope`, `ScopeContext`, `ResourceClass`, `ResourceKind`). Service-level message types (discovery, control-plane, and scheduler messages, deployment and pipeline desired-state records, resource and interface metadata, and pipe-control frames) SHALL NOT be defined in `selium-abi`; they live in `selium-service`, which depends on `selium-abi` for the shared vocabulary.

#### Scenario: Service messages are not part of the ABI

- **WHEN** a consumer wants a service message such as `DiscoveryRequest` or `ControlResponse`
- **THEN** `selium-abi` SHALL NOT export it
- **AND** the consumer SHALL import it from `selium-service`

#### Scenario: Hostcall contract is unchanged

- **WHEN** a guest encodes `HostcallRequest` or `HostcallEnvelope`
- **THEN** it SHALL use the rkyv helpers in `selium-abi` exactly as before

### Requirement: Certificate Signing Hostcall Variants

`HostcallRequest` SHALL define certificate-signing variants `SignTenantCa` (sign a generated tenant CA keypair via the online intermediate), `SignUserCert` (sign a client-supplied SPKI via that tenant's CA key), and `RevokeCa` (delete a tenant CA key from the keyring). The corresponding `HostcallOutput` variants SHALL carry DER-encoded public certificates only and SHALL NOT carry private-key material.

#### Scenario: SignTenantCa round-trips

- **WHEN** a guest encodes `HostcallRequest::SignTenantCa` and the runtime processes it
- **THEN** the hostcall SHALL complete with a `HostcallOutput` variant carrying the signed tenant CA certificate

#### Scenario: SignUserCert round-trips

- **WHEN** a guest encodes `HostcallRequest::SignUserCert` with a client SPKI and tenant
- **THEN** the hostcall SHALL complete with a `HostcallOutput` variant carrying the signed leaf certificate

#### Scenario: RevokeCa returns no key material

- **WHEN** a guest encodes `HostcallRequest::RevokeCa` for a tenant
- **THEN** the hostcall SHALL complete and the tenant's CA key SHALL be removed from the keyring

### Requirement: MintCertificate Capability Variant

`Capability` SHALL include a `MintCertificate` variant that gates the certificate-signing hostcalls. The variant SHALL be rkyv-encodable like the other capability variants.

#### Scenario: Signing hostcall requires the capability

- **WHEN** a process without a `MintCertificate` grant invokes a certificate-signing hostcall
- **THEN** the runtime SHALL deny the hostcall with a capability error

### Requirement: Resolved-Resource Recording Hostcall Variants

`HostcallRequest` SHALL define recording variants `RecordResolvedQueueFor` and `RecordResolvedRegionFor`, each naming a client process and the resource id a discovery Resolve returned to it. Both SHALL be accepted only from the discovery system guest; any other caller SHALL be denied with a capability error. A recorded id SHALL give the named client process an authorisation basis for the corresponding cross-process attach hostcall (`HostQueueAttach` for queues, `AttachRegion` for shared regions) on a resource it did not allocate — the basis peer guests use to attach a publishing guest's live tables.

#### Scenario: Non-discovery caller is denied

- **WHEN** a process other than the discovery system guest invokes `RecordResolvedQueueFor` or `RecordResolvedRegionFor`
- **THEN** the runtime SHALL deny the hostcall with a capability error

#### Scenario: Recorded id authorises the attach

- **WHEN** the discovery system guest records a region id resolved by a client process
- **THEN** that client process's subsequent `AttachRegion` on the region SHALL be authorised

### Requirement: Quota Hostcall Variants

`HostcallRequest` SHALL define `QuotaSet { tenant, class, limit }` and `QuotaClear { tenant, class }`, rkyv-encoded like all hostcall requests. `QuotaSet` SHALL store a quota counter for the tenant and resource class; `QuotaClear` SHALL remove it. The corresponding `HostcallOutput` SHALL return unit. A quota denial (an allocation or enqueue that would exceed the tenant's authored ceiling) SHALL fail with the `AbiErrorCode::QuotaExceeded` error code — distinct from `PermissionDenied` — so guests can tell "you lack the capability" apart from "your tenant hit its ceiling"; the message SHALL name the tenant and resource class.

#### Scenario: QuotaSet round-trips

- **WHEN** a guest encodes `HostcallRequest::QuotaSet` with a tenant, resource class, and limit
- **THEN** the hostcall SHALL complete and the host SHALL store the counter

#### Scenario: QuotaClear removes the counter

- **WHEN** a guest encodes `HostcallRequest::QuotaClear` for a tenant and class
- **THEN** the hostcall SHALL complete and the stored counter SHALL be removed

### Requirement: QuotaWrite Capability Variant

`Capability` SHALL include a `QuotaWrite` variant that gates the quota hostcalls. The variant SHALL be rkyv-encodable like the other capability variants.

#### Scenario: Quota hostcall requires the capability

- **WHEN** a process without a `QuotaWrite` grant invokes a quota hostcall
- **THEN** the runtime SHALL deny the hostcall with a capability error

### Requirement: Process Metering Observation

`selium-abi` SHALL define `MeteringObservation` with fields `cpu_instructions: u64` (cumulative executed instructions), `memory_bytes: u64` (current committed linear-memory gauge), `storage_bytes: u64` (durable storage gauge), and `bandwidth_bytes: u64` (cumulative network bytes). The type SHALL NOT define a `cpu_micros` field.

#### Scenario: Observation fields round-trip

- **WHEN** a `MeteringObservation` is rkyv-encoded and decoded
- **THEN** `cpu_instructions`, `memory_bytes`, `storage_bytes`, and `bandwidth_bytes` SHALL survive unchanged

#### Scenario: Cpu microseconds absent

- **WHEN** a consumer reads a `MeteringObservation`
- **THEN** there SHALL be no `cpu_micros` field to read

### Requirement: CPU Resource Class

`selium-abi` SHALL include `Cpu` in the `ResourceClass` enum so that a tenant's per-minute CPU instruction ceiling can be carried through the shared quota vocabulary as a quota dimension (not an allocatable resource).

#### Scenario: Cpu variant joins the resource vocabulary

- **WHEN** a resource class is converted to and from its URI segment
- **THEN** `ResourceClass::Cpu` SHALL round-trip through the segment `cpu` like every other class

#### Scenario: Cpu class allocates no resource

- **WHEN** a guest allocates or acquires any resource
- **THEN** nothing SHALL consume a `ResourceClass::Cpu` allocation, because the class only carries the CPU instruction ceiling
