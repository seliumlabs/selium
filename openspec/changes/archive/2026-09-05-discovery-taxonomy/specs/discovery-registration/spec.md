## REMOVED Requirements

### Requirement: Runtime-authoritative ownership registration
**Reason**: The `sel://process/<id>/` prefix and the `(process_id, resource_id)` ownership table it populated are replaced by tenant-scoped registration under principal provenance — resources are minted under a serving tenant, not keyed by the allocating process's id in the URI. There is no process prefix left to extract an owner from.
**Migration**: Register resources as `sel://<tenant>/<type>/<id>`; Tier-2 validation gates on the registering process's own tenant and on ownership of the target resource, which the runtime records separately from the URI.

## MODIFIED Requirements

### Requirement: Guest custom URI validation
A guest (Tier-2) SHALL be permitted to register a URI only within the guest's own tenant (a non-root tenant) and only for a target resource the guest owns. Registration into the root tenant (empty tenant) SHALL be rejected. A guest MAY register leaf aliases for a target it owns, under its own tenant. A leaf alias SHALL be accepted only when the claimed target's typed registration currently exists and the claimed class matches the class of the resource the caller owns; external names SHALL meet the same class-match requirement.

#### Scenario: Guest registers custom URI for owned resource
- **WHEN** a guest (tenant "acme", process 42) sends `DiscoveryRequest::Register` for a URI under `sel://acme/` targeting a resource id the guest's process owns
- **THEN** the discovery service SHALL store the mapping and respond `DiscoveryResponse::Registered`

#### Scenario: Guest registration rejected for unowned resource
- **WHEN** a guest (process 42) sends `DiscoveryRequest::Register` for a URI targeting a resource the guest's process does not own
- **THEN** the discovery service SHALL respond `DiscoveryResponse::Forbidden`
- **AND** the mapping SHALL NOT be stored

#### Scenario: Guest registration rejected for class mismatch
- **WHEN** a guest that owns resource 7 as a shared region registers an alias or external name claiming resource 7 is of a different class (for example a process node)
- **THEN** the discovery service SHALL respond `DiscoveryResponse::Forbidden`

#### Scenario: Guest alias rejected for unregistered target
- **WHEN** a guest registers a leaf alias whose canonical typed target is not currently registered (for example the target was revoked, or lives under a different tenant)
- **THEN** the discovery service SHALL respond `DiscoveryResponse::NotFound`
- **AND** the alias SHALL NOT be stored

#### Scenario: Guest revokes their own custom URI
- **WHEN** a guest sends `DiscoveryRequest::Revoke` for a URI they previously registered under their own tenant
- **THEN** the discovery service SHALL remove the mapping and respond `DiscoveryResponse::Revoked`

### Requirement: Tenant-scoped process URI resolution
Resolving a typed URI (`sel://<tenant>/proc/<id>`, `sel://<tenant>/region/<id>`, and so on) SHALL be tenant-scoped: the discovery service SHALL compare the target's tenant against the calling process's tenant, obtained from the RPC connection metadata. A caller resolving another tenant's resource SHALL receive `NotFound`. When either tenant is absent (a verified `None`, i.e. a root/system principal), the check SHALL be skipped for backward compatibility. A **failed** tenant lookup SHALL NOT be treated as absence: the service SHALL fail closed, refusing writes (`Forbidden`) and disclosing nothing on reads (`NotFound` or an empty result set).

#### Scenario: Same-tenant guest resolves process URI
- **WHEN** a guest in tenant "acme" resolves `sel://acme/proc/42` and the target belongs to tenant "acme"
- **THEN** the discovery service SHALL return `DiscoveryResponse::Found`

#### Scenario: Cross-tenant guest cannot resolve process URI
- **WHEN** a guest in tenant "beta" resolves `sel://acme/proc/42`
- **THEN** the discovery service SHALL return `DiscoveryResponse::NotFound`

#### Scenario: Failed tenant lookup fails closed
- **WHEN** the discovery service cannot resolve the calling process's tenant from the runtime
- **THEN** the service SHALL deny the request (`Forbidden` for Register/Revoke, `NotFound` or an empty set for queries) rather than treating the caller as unscoped

## ADDED Requirements

### Requirement: Deterministic Tenant-Scoped URI Schema
Internal addressing SHALL use a single deterministic URI schema: `sel://<tenant>/<type>/<id>`, where `<tenant>` is the URI authority (empty for the root/system tenant), `<type>` names a resource class from the closed set, and `<id>` is the resource's numeric identity. Arbitrary user-defined path hierarchy SHALL NOT be used.

#### Scenario: Internal address is typed and tenant-scoped
- **WHEN** region 456 belonging to tenant "acme" is registered
- **THEN** resolving `sel://acme/region/456` SHALL return that region's target

#### Scenario: Process address
- **WHEN** process 123 belonging to tenant "acme" is registered
- **THEN** resolving `sel://acme/proc/123` SHALL return that process's target

### Requirement: Root Tenant Namespace
The empty tenant (`sel:///…`) SHALL be the root/system namespace, reserved to the runtime and system guests (Tier-1). Guest registration SHALL require a non-root tenant, so the root namespace cannot be registered, aliased, or taken over by a guest.

#### Scenario: Guest cannot register the root namespace
- **WHEN** a guest sends `DiscoveryRequest::Register` for a URI under `sel:///`
- **THEN** the discovery service SHALL respond `DiscoveryResponse::Forbidden` and store nothing

#### Scenario: System guest registers under root
- **WHEN** the runtime registers a system guest's channel at `sel:///dns/resolve`
- **THEN** resolving `sel:///dns/resolve` SHALL return its target

### Requirement: Leaf Aliases
A single-segment name under a tenant (`sel://<tenant>/<name>`) SHALL be registrable as a leaf alias resolving to a typed target `(type, id)`. An alias SHALL NOT name another alias. When a target is revoked, every alias resolving to that target SHALL also be revoked.

#### Scenario: Alias resolves to a typed target
- **WHEN** the alias `sel://acme/proxy` is registered pointing at `(proc, 123)`
- **THEN** resolving `sel://acme/proxy` SHALL return the same target as `sel://acme/proc/123`

#### Scenario: Revoking a target revokes its aliases
- **WHEN** region 456, which has a registered alias, is revoked
- **THEN** resolving that alias SHALL return `NotFound`

### Requirement: Classification Labels
Each resource target SHALL carry zero or more key/value labels. The discovery service SHALL answer label queries by returning every target in the caller's tenant whose labels match the queried key/value pair.

#### Scenario: Label query returns matching targets
- **WHEN** processes 123 and 124 are labelled `app=web` and process 125 is not
- **THEN** a label query for `app=web` in tenant "acme" SHALL return processes 123 and 124 and SHALL NOT return process 125

### Requirement: Prefix and Enumeration Queries
The discovery service SHALL support prefix listing under the typed schema, for example `sel://acme/region/*`, returning every matching target in the caller's tenant. Enumeration SHALL be tenant-scoped: a caller SHALL NOT list resources of another tenant.

#### Scenario: Prefix lists a tenant's resources
- **WHEN** a guest in tenant "acme" queries `sel://acme/region/*`
- **THEN** the discovery service SHALL return all region targets of tenant "acme"

#### Scenario: Cross-tenant enumeration denied
- **WHEN** a guest in tenant "beta" queries `sel://acme/region/*`
- **THEN** the discovery service SHALL NOT return tenant "acme"'s targets

### Requirement: External Name Registry
External bindings SHALL register their public address as an opaque name-to-target key, for example `https://acme.com/path/`, or a bare hostname for server-name-only protocols. The discovery service SHALL store and match external names opaquely, without interpreting their scheme or path; the serving connector is responsible for normalizing incoming addresses to the registered key.

#### Scenario: External address resolves
- **WHEN** a connector registers `https://acme.com/path/` as the key for a serving guest
- **THEN** resolving that key SHALL return the serving guest's target

#### Scenario: Opaque matching is exact after normalization
- **WHEN** the connector normalizes an incoming address to the canonical registered key
- **THEN** the lookup SHALL match, and a differently-but-non-equivalently-spelled key SHALL NOT match

### Requirement: Process Node Addressing
Every process SHALL be addressable as a typed entry `sel://<tenant>/proc/<id>` from spawn until teardown, even when the process has allocated no resources.

#### Scenario: A process without resources is addressable
- **WHEN** process 123 of tenant "acme" is spawned and allocates nothing
- **THEN** resolving `sel://acme/proc/123` SHALL return the process's target

### Requirement: Guest Revocation Authorization
A guest (Tier-2) SHALL be permitted to revoke only custom registrations — leaf aliases and opaque external names — whose target resource the guest owns, and only within the guest's own tenant. Revocation of a typed URI over Tier-2 SHALL be rejected (`Forbidden`): typed URIs are runtime-minted and revoked over the Tier-1 feed. Revocation of an unknown key SHALL return `NotFound`.

#### Scenario: Guest revokes own external name
- **WHEN** a guest that owns the target resource sends `DiscoveryRequest::Revoke` for an external name it registered
- **THEN** the discovery service SHALL remove the mapping and respond `DiscoveryResponse::Revoked`

#### Scenario: Guest cannot revoke a typed URI
- **WHEN** a guest sends `DiscoveryRequest::Revoke` for `sel://acme/region/7`
- **THEN** the discovery service SHALL respond `DiscoveryResponse::Forbidden`
- **AND** the registration SHALL remain resolvable

#### Scenario: Guest cannot revoke another tenant's alias
- **WHEN** a guest whose verified tenant is "beta" sends `DiscoveryRequest::Revoke` for `sel://acme/proxy`
- **THEN** the discovery service SHALL respond `DiscoveryResponse::Forbidden`
- **AND** the alias SHALL remain resolvable

#### Scenario: Guest cannot revoke a registration it does not own
- **WHEN** a guest sends `DiscoveryRequest::Revoke` for an alias or external name whose target resource the guest does not own
- **THEN** the discovery service SHALL respond `DiscoveryResponse::Forbidden`
- **AND** the registration SHALL remain resolvable

#### Scenario: Revoking an unknown key returns NotFound
- **WHEN** a guest sends `DiscoveryRequest::Revoke` for a key that is not registered
- **THEN** the discovery service SHALL respond `DiscoveryResponse::NotFound`
