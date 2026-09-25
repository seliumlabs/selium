## Purpose

Discovery registration enables Selium guests and the runtime to register, revoke, and resolve URI-to-resource mappings through the discovery service, using a single deterministic tenant-scoped URI schema (`sel://<tenant>/<type>/<id>`), tenant-scoped validation of guest custom registrations, and tenant-scoped resolution that fails closed.

## Requirements

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

### Requirement: Serve-based route registration
`Context` SHALL provide a `serve` method that registers a named route from a resource the guest created itself. The route SHALL derive from a path (for example `["bridge"]`) plus the guest's tenant, producing both the internal path (`sel://<tenant>/bridge`) and the wire name (`bridge.<tenant>`, or `bridge.<owned-domain>` when the domain table maps the tenant). The method SHALL optionally accept a root-service flag for apex aliasing.

#### Scenario: Guest registers its own route
- **WHEN** a guest calls `ctx.serve(Serve { path: ["bridge"], target, default: false }).await`
- **THEN** discovery SHALL store the route resolvable as `sel://<tenant>/bridge` and `bridge.<tenant>`

#### Scenario: Route registration derives wire name from the domain table
- **WHEN** the guest's tenant owns a registered domain and serves a path
- **THEN** the wire name under that domain SHALL also resolve to the route

### Requirement: Root registration capability-gated
Registration in the root/system tenant SHALL be permitted only when the guest holds the corresponding registration capability, replacing the runtime's special-case well-known URI provisioning.

#### Scenario: Root registration requires capability
- **WHEN** a guest without the system-registration capability attempts to register in the root tenant
- **THEN** the request SHALL be forbidden

#### Scenario: Capability-holding guest registers in root
- **WHEN** a guest holding the system-registration capability registers in the root tenant
- **THEN** the registration SHALL be accepted

### Requirement: FlatBuffers Tier-1 Discovery Feed
The runtime→discovery (Tier-1) event feed SHALL carry FlatBuffers-encoded `DiscoveryRequest` values. The runtime SHALL publish typed `DiscoveryRequest` values (encoded via `FlatMsg`) instead of raw rkyv bytes, and the discovery guest SHALL subscribe to and decode typed `DiscoveryRequest` values via `FlatMsg`. The feed representation SHALL cover every Tier-1 operation — `Register` (including an optional owner), `Revoke`, `RevokeByOwner`, and `SeedDomain` — without lossy collapse onto an unrelated variant.

#### Scenario: Process-node registration round-trips over the feed
- **WHEN** the runtime publishes a `DiscoveryRequest::Register` for a spawned process
- **THEN** the discovery guest SHALL decode the same registration, preserving `uri`, `target`, and `owner`

#### Scenario: Revoke-by-owner round-trips over the feed
- **WHEN** the runtime publishes `DiscoveryRequest::RevokeByOwner { process_id }`
- **THEN** the feed SHALL carry the operation with its `process_id` intact
- **AND** the discovery guest SHALL decode it as `RevokeByOwner` rather than another variant

#### Scenario: Seed-domain round-trips over the feed
- **WHEN** the runtime publishes `DiscoveryRequest::SeedDomain { domain, tenant }`
- **THEN** the feed SHALL carry both `domain` and `tenant` intact to the discovery guest

#### Scenario: Feed is not rkyv-encoded
- **WHEN** the runtime publishes to or the discovery guest reads from the Tier-1 feed
- **THEN** neither side SHALL use `selium_abi::encode_rkyv` or `decode_rkyv` for the feed payload
