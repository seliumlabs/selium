## MODIFIED Requirements

### Requirement: Automatic resource registration on allocation
When the runtime dispatches an `AllocRegion` hostcall, it SHALL send `DiscoveryRequest::Register` for the allocated region under `sel://<tenant>/region/<id>`, where `<tenant>` is the allocation's principal tenant (the tenant on whose behalf the region is minted). The runtime SHALL NOT auto-register purpose aliases; aliases are registered explicitly by the resource's owner.

#### Scenario: Runtime registers log channel on AllocRegion
- **WHEN** a guest invokes `AllocRegion { purpose: LogChannel, ... }`, the runtime allocates region 7, and the allocation's principal tenant is "acme"
- **THEN** the runtime SHALL register `sel://acme/region/7` and SHALL NOT register a purpose alias

#### Scenario: Runtime registers generic SharedMemory region
- **WHEN** a guest invokes `AllocRegion { purpose: SharedMemory, ... }` and the runtime allocates region 3 under tenant "acme"
- **THEN** the runtime SHALL register `sel://acme/region/3`

### Requirement: Process Teardown Revocation
When a process exits, the runtime SHALL publish Tier-1 revocation events for the process's region URIs and for its process node before reclaiming its resources. Teardown bookkeeping SHALL be staged: a revocation's pending entry SHALL be removed only after its publish succeeds, so a failed teardown loses no bookkeeping. A stop that fails part-way SHALL retain the process authority so the stop can be retried and the remaining revocations completed, rather than silently skipping them.

#### Scenario: Exit revokes before reclaim
- **WHEN** a process with allocated regions is stopped
- **THEN** revocation events for its region URIs SHALL be published to the discovery feed before its shared resources are reclaimed

#### Scenario: Runtime revokes all process URIs on exit
- **WHEN** process 42 of tenant "acme" terminates
- **THEN** the runtime SHALL revoke `sel://acme/region/*` for the regions it allocated and SHALL revoke `sel://acme/proc/42`
- **AND** subsequent `Resolve` calls for those URIs SHALL return `NotFound`

#### Scenario: Failed teardown is retryable
- **WHEN** a process stop fails part-way because a discovery revocation cannot be published
- **THEN** the stop SHALL return an error (not succeed silently), the pending revocations SHALL remain staged, and a later stop of the same process SHALL publish them

### Requirement: Well-Known Connector Channel Provisioning
`selium-runtime` SHALL provision the well-known channel of a system guest whose descriptor declares a well-known URI (for example the DNS connector's `sel:///dns/resolve`): it SHALL create the host listener queue, inject the queue's shared id as the leading entrypoint argument, grant the guest attach rights for it, register the URI with discovery at provision time, and publish a revocation for the URI when the guest terminates. A well-known guest SHALL NOT also receive the discovery handle argument.

#### Scenario: Well-known channel provisioned at spawn time
- **WHEN** a system guest descriptor declares a well-known URI under the root tenant
- **THEN** the runtime SHALL create a host listener queue, pass its shared id as the first entrypoint argument, grant the guest attach rights for it, and publish a discovery `Register` for the URI targeting that queue

#### Scenario: Well-known URI revoked at teardown
- **WHEN** the guest serving a well-known URI terminates
- **THEN** the runtime SHALL publish a discovery `Revoke` for that URI before reclaiming the process's resources

## ADDED Requirements

### Requirement: Process Node Registration
The runtime SHALL publish a Tier-1 registration for a process node `sel://<tenant>/proc/<id>` when a process is spawned, and a revocation when the process is cleaned up, so every process is discoverable even before it allocates any resource.

#### Scenario: Process node registered at spawn
- **WHEN** process 123 of tenant "acme" is spawned
- **THEN** the runtime SHALL register `sel://acme/proc/123` so it resolves through discovery

#### Scenario: Process node revoked at teardown
- **WHEN** process 123 exits
- **THEN** the runtime SHALL revoke `sel://acme/proc/123` so it no longer resolves

### Requirement: Principal-Provenance Allocation
`AllocRegion` and `HostQueueCreate` SHALL mint the resource under the serving tenant supplied with the allocation, which MAY differ from the allocating process's own tenant. A root principal (a process with no tenant) MAY mint for any tenant — trusted edge infrastructure such as connectors runs as root. A tenant-scoped process MAY mint for another tenant only with a `DelegateGrants` grant scoped to that tenant; allocation crossing a tenant boundary without that authority SHALL be denied with `AbiErrorCode::PermissionDenied`. A `DelegateGrants` grant SHALL carry at least one `Tenant` selector; a selector-less `DelegateGrants` grant would vacuously match every tenant (including root) and SHALL be rejected at spawn.

#### Scenario: System process allocates for another tenant
- **WHEN** a root/system process holding tenant-scoped delegation allocates a region on behalf of tenant "acme"
- **THEN** the runtime SHALL register the region under `sel://acme/region/<id>`

#### Scenario: Root principal allocates for any tenant without delegation
- **WHEN** a root process with no `DelegateGrants` grants allocates a region on behalf of tenant "acme" (the connector path: a root connector minting per-stream channels under an authenticated client's tenant)
- **THEN** the runtime SHALL register the region under `sel://acme/region/<id>`

#### Scenario: Unauthorized cross-tenant allocation denied
- **WHEN** a tenant-scoped process without tenant-scoped delegation attempts to allocate a region for a tenant other than its own
- **THEN** the runtime SHALL return `AbiErrorCode::PermissionDenied`

#### Scenario: Host queue minted under the serving tenant
- **WHEN** a guest in tenant "acme" creates a host queue with no serving tenant, the queue is registered under `sel://acme/queue/<id>`; when a cross-tenant mint is authorized, the queue is registered under the serving tenant
- **THEN** teardown SHALL revoke the queue under the tenant it was minted for

#### Scenario: Selector-less delegation grant rejected
- **WHEN** a system guest descriptor carries a `DelegateGrants` grant with no `Tenant` selector
- **THEN** the runtime SHALL reject the spawn with an invalid-grant error
