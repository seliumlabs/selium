## ADDED Requirements

### Requirement: Discovery URI registration
The discovery service SHALL accept `DiscoveryRequest::Register { uri, target }` and store the mapping in its registry. It SHALL accept `DiscoveryRequest::Revoke { uri }` and remove the mapping. Both SHALL respond with a confirmation: `DiscoveryResponse::Registered` or `DiscoveryResponse::Revoked`.

#### Scenario: Caller registers a URI
- **WHEN** a caller sends `DiscoveryRequest::Register { uri: "sel://tenant/logs/app", target }` to the discovery service
- **THEN** the discovery service SHALL store the mapping and respond with `DiscoveryResponse::Registered`
- **AND** subsequent `DiscoveryRequest::Resolve("sel://tenant/logs/app")` SHALL return `DiscoveryResponse::Found(target)`

#### Scenario: Caller revokes a URI
- **WHEN** a caller sends `DiscoveryRequest::Revoke { uri: "sel://tenant/logs/app" }` to the discovery service
- **THEN** the discovery service SHALL remove the mapping and respond with `DiscoveryResponse::Revoked`
- **AND** subsequent `DiscoveryRequest::Resolve("sel://tenant/logs/app")` SHALL return `DiscoveryResponse::NotFound`

#### Scenario: Register overwrites existing URI
- **WHEN** a caller registers a URI that is already mapped
- **THEN** the new target SHALL replace the existing mapping
- **AND** the response SHALL be `DiscoveryResponse::Registered`

#### Scenario: Revoke on unknown URI
- **WHEN** a caller revokes a URI that is not registered
- **THEN** the discovery service SHALL respond with `DiscoveryResponse::Revoked` (idempotent)

### Requirement: Runtime-authoritative ownership registration
The discovery service SHALL maintain an ownership table mapping `(process_id, resource_id)` pairs, populated by the runtime via privileged `DiscoveryRequest::Register` calls. A `Register` call with a `uri` prefixed `sel://process/<id>/` SHALL be treated as authoritative and SHALL populate the ownership table. A `Revoke` call to any URI SHALL also remove the corresponding ownership entry.

#### Scenario: Runtime registers a process resource
- **WHEN** the runtime sends `DiscoveryRequest::Register { uri: "sel://process/42/regions/7", target }`
- **THEN** the discovery service SHALL store the mapping AND record that process 42 owns resource 7

#### Scenario: Runtime revokes all process resources on termination
- **WHEN** the runtime sends `DiscoveryRequest::Revoke` for each URI under `sel://process/42/`
- **THEN** all ownership entries for process 42 SHALL be removed

### Requirement: Guest custom URI validation
When a guest (Tier 2) sends `DiscoveryRequest::Register { uri, target }`, the discovery service SHALL check whether the `client_process_id` on the RPC connection owns `target.resource_id` according to the ownership table. If the process does not own the resource, the service SHALL respond with `DiscoveryResponse::Forbidden`.

#### Scenario: Guest registers custom URI for owned resource
- **WHEN** a guest (process 42) sends `DiscoveryRequest::Register { uri: "sel://my-app/logs", target }` and the ownership table shows process 42 owns `target.resource_id`
- **THEN** the discovery service SHALL store the mapping and respond with `DiscoveryResponse::Registered`

#### Scenario: Guest registration rejected for unowned resource
- **WHEN** a guest (process 42) sends `DiscoveryRequest::Register { uri: "sel://my-app/logs", target }` and the ownership table does NOT show process 42 owning `target.resource_id`
- **THEN** the discovery service SHALL respond with `DiscoveryResponse::Forbidden`
- **AND** the mapping SHALL NOT be stored

#### Scenario: Guest revokes their own custom URI
- **WHEN** a guest sends `DiscoveryRequest::Revoke { uri: "sel://my-app/logs" }` for a URI they previously registered
- **THEN** the discovery service SHALL remove the mapping and respond with `DiscoveryResponse::Revoked`

### Requirement: Tenant-scoped process URI resolution
`DiscoveryRequest::Resolve` for URIs under `sel://process/<id>/` SHALL only return `Found` if the requesting guest belongs to the same tenant as process `<id>`. Cross-tenant resolution SHALL return `NotFound`.

#### Scenario: Same-tenant guest resolves process URI
- **WHEN** a guest in tenant A resolves `sel://process/42/logs` and process 42 belongs to tenant A
- **THEN** the discovery service SHALL return `DiscoveryResponse::Found`

#### Scenario: Cross-tenant guest cannot resolve process URI
- **WHEN** a guest in tenant B resolves `sel://process/42/logs` and process 42 belongs to tenant A
- **THEN** the discovery service SHALL return `DiscoveryResponse::NotFound`

### Requirement: Context convenience methods for registration
`Context` SHALL provide `register(&mut self, uri: &str, target: ResourceTarget) -> Result<(), GuestError>` and `revoke(&mut self, uri: &str) -> Result<(), GuestError>` convenience methods that delegate to the discovery RPC client.

#### Scenario: Context::register sends Register request
- **WHEN** a guest calls `ctx.register("sel://tenant/logs/app", target).await`
- **THEN** the method SHALL send `DiscoveryRequest::Register { uri: "sel://tenant/logs/app", target }` via the RPC client and return `Ok(())` on `DiscoveryResponse::Registered`

#### Scenario: Context::register returns error on Forbidden
- **WHEN** a guest calls `ctx.register(uri, target).await` and the discovery service responds with `DiscoveryResponse::Forbidden`
- **THEN** the method SHALL return `Err(GuestError::Host("registration forbidden: process does not own resource"))`

#### Scenario: Context::revoke sends Revoke request
- **WHEN** a guest calls `ctx.revoke("sel://tenant/logs/app").await`
- **THEN** the method SHALL send `DiscoveryRequest::Revoke { uri: "sel://tenant/logs/app" }` via the RPC client and return `Ok(())` on `DiscoveryResponse::Revoked`
