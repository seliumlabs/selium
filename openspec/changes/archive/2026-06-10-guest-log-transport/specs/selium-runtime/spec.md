## ADDED Requirements

### Requirement: Runtime discovery RPC session
`selium-runtime` SHALL hold an `RpcClient<DiscoveryRequest, DiscoveryResponse>` connected to the discovery guest, established during bootstrap alongside the existing discovery queue for guest `Context` connections. This session SHALL be used for authoritative Tier-1 resource registration.

#### Scenario: Runtime connects to discovery guest
- **WHEN** the runtime bootstraps
- **THEN** it SHALL create an `RpcClient` to the discovery guest for authoritative resource registration

### Requirement: Automatic resource registration on allocation
When the runtime dispatches an `AllocRegion` hostcall, it SHALL send `DiscoveryRequest::Register` to the discovery guest for:
1. `sel://process/<process_id>/regions/<region_id>` — always, for every allocation
2. A purpose-specific alias if the `purpose` field maps to a known URI pattern (e.g., `sel://process/<process_id>/logs` for `ResourceKind::LogChannel`, `sel://process/<process_id>/tables/<name>` for `ResourceKind::LiveTable`)

#### Scenario: Runtime registers log channel on AllocRegion
- **WHEN** a guest invokes `AllocRegion { purpose: LogChannel, ... }` and the runtime allocates region 7 for process 42
- **THEN** the runtime SHALL register `sel://process/42/regions/7` AND `sel://process/42/logs` with the discovery service

#### Scenario: Runtime registers generic SharedMemory region
- **WHEN** a guest invokes `AllocRegion { purpose: SharedMemory, ... }` and the runtime allocates region 3 for process 42
- **THEN** the runtime SHALL register `sel://process/42/regions/3` (no purpose alias for generic regions)

### Requirement: Automatic resource revocation on process termination
When a guest process terminates, the runtime SHALL send `DiscoveryRequest::Revoke` for every URI registered under `sel://process/<process_id>/`. This SHALL include both the `regions/<id>` entries and all purpose-specific aliases.

#### Scenario: Runtime revokes all process URIs on exit
- **WHEN** process 42 terminates
- **THEN** the runtime SHALL revoke `sel://process/42/regions/*` and all purpose aliases (e.g., `sel://process/42/logs`, `sel://process/42/tables/*`)
- **AND** subsequent `Resolve` calls for those URIs SHALL return `NotFound`

### Requirement: GuestLogRegister hostcall validation
The runtime SHALL validate that the `shared_id` in a `GuestLogRegister` hostcall was allocated by the calling process. If the `shared_id` belongs to a different process, the runtime SHALL return an error.

#### Scenario: GuestLogRegister accepted for own region
- **WHEN** process 42 sends `GuestLogRegister { shared_id }` and `shared_id` corresponds to a region allocated by process 42
- **THEN** the runtime SHALL attach to the region as a log reader and return success

#### Scenario: GuestLogRegister rejected for foreign region
- **WHEN** process 42 sends `GuestLogRegister { shared_id }` and `shared_id` corresponds to a region allocated by process 99
- **THEN** the runtime SHALL return an error without attaching

### Requirement: Discovery handle passed to guest entrypoints
The runtime SHALL continue to pass the discovery host queue `shared_id` to guest entrypoints for `Context::from_raw` (existing behaviour, unchanged). The runtime's own authoritative discovery RPC session SHALL be separate from the guest-facing discovery queue.

#### Scenario: Application guest receives discovery handle (unchanged)
- **WHEN** the runtime bootstraps an application guest
- **THEN** the guest's entrypoint SHALL receive the discovery `shared_id` as a u64 argument for `Context::from_raw`
