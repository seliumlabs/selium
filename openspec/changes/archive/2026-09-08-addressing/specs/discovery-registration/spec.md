## MODIFIED Requirements

### Requirement: Discovery URI registration
The discovery service SHALL accept `DiscoveryRequest::Register { uri, target }`, record the registering process as the route's owner, and store the mapping in its registry. It SHALL accept `DiscoveryRequest::Revoke { uri }` and remove the mapping. Both SHALL respond with a confirmation: `DiscoveryResponse::Registered` or `DiscoveryResponse::Revoked`.

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

#### Scenario: Owner revoked on exit
- **WHEN** the process that owns a registration exits or is killed
- **THEN** the discovery service SHALL revoke the process's registrations without a runtime-maintained map of well-known URIs

## REMOVED Requirements

### Requirement: External Name Registry
Replaced by the unified wire-name resolution in the `addressing` capability: external names are no longer opaque keys matched exactly; they derive tenant and path through the domain table and label reversal.

## ADDED Requirements

### Requirement: External name resolution through addressing
The discovery service SHALL resolve external wire names through the unified addressing resolver: derive the tenant from the domain-to-tenant table (or the synthetic tenant label), reverse the remaining labels into a path, and resolve that path. Resolution SHALL fail closed for unknown tenants or paths.

#### Scenario: Wire name resolves to a tenant route
- **WHEN** discovery resolves `bridge.acme`
- **THEN** it SHALL derive tenant `acme`, reverse the label into path `["bridge"]`, and resolve `sel://acme/bridge`

#### Scenario: Registered domain wire name resolves
- **WHEN** discovery resolves `bridge.example.com` and `example.com -> acme` is provisioned
- **THEN** it SHALL resolve `sel://acme/bridge`

### Requirement: Advisory domain scoping
The discovery service SHALL consult the advisory domain-to-tenant table when accepting registrations under a domain, refusing a registration under a domain not owned by the registering tenant. The table SHALL NOT be treated as an authentication boundary.

#### Scenario: Tenant registers within its domain
- **WHEN** a tenant owning `example.com` registers a service
- **THEN** the registration SHALL produce the wire name under `example.com`

#### Scenario: Foreign domain registration refused
- **WHEN** a tenant attempts to register a name under a domain it does not own
- **THEN** the discovery service SHALL refuse the registration
