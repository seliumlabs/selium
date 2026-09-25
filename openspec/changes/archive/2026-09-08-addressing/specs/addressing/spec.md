## ADDED Requirements

### Requirement: Single Canonical Namespace
The system SHALL have exactly one canonical address space rooted at `sel://<tenant>/<segments...>`. External wire names SHALL be projections of this space, not a second namespace. Every addressable resource SHALL be identified by a single internal path.

#### Scenario: Internal path is canonical
- **WHEN** a resource is registered
- **THEN** it SHALL be registered under exactly one internal path, and external names for it SHALL derive from that path

#### Scenario: No parallel namespace
- **WHEN** a connector resolves a wire name
- **THEN** it SHALL resolve through the internal path space rather than a separate external key space

### Requirement: Path-Label Reversal
A named service's wire name SHALL be its internal path segments reversed, joined with dots, appended to the tenant's domain. The synthetic domain (`<tenant>`) SHALL always be resolvable cluster-internally; a registered domain SHALL be used when the tenant owns one.

#### Scenario: Bridge route projects to a wire name
- **WHEN** the tenant `acme` serves the path `["bridge"]`
- **THEN** the synthetic wire name `bridge.acme` SHALL resolve to it, and, when `acme` owns `example.com`, `bridge.example.com` SHALL resolve to it

#### Scenario: Deeper path reverses in full
- **WHEN** the tenant `acme` serves the path `["http", "prod"]`
- **THEN** the wire name SHALL be `prod.http.acme` (synthetic) or `prod.http.example.com` (registered)

### Requirement: Advisory Domain-to-Tenant Table
The system SHALL maintain a domain-to-tenant mapping (`example.com -> acme`) provisioned out-of-band. The mapping SHALL be used for routing and for scoping which tenant may register names under a domain. The mapping SHALL NOT be an authentication source.

#### Scenario: Provisioned domain routes to its tenant
- **WHEN** a wire name ends in a registered domain
- **THEN** the resolver SHALL derive the tenant from the mapping and resolve the reversed labels within that tenant's namespace

#### Scenario: Mapping is advisory, not authoritative for identity
- **WHEN** a client connects using a wire name under a domain
- **THEN** the client's identity SHALL still be established by its certificate, and the tenant SHALL still refuse cross-tenant identities

#### Scenario: Synthetic tenant label always resolves
- **WHEN** a wire name uses the synthetic `<tenant>` label
- **THEN** the resolver SHALL resolve it without any provisioned domain entry

### Requirement: Unified Wire-Name Resolution
The system SHALL provide a single resolver that maps a wire name to a route by: stripping the tenant domain to derive the tenant, reversing the remaining labels into a path, and resolving that path via discovery. QUIC, HTTP, and other edge protocols SHALL share this resolver.

#### Scenario: SNI resolved through the shared resolver
- **WHEN** the QUIC connector resolves a connection's SNI
- **THEN** it SHALL use the unified resolver, not a flat external-name lookup

#### Scenario: Host resolved through the shared resolver
- **WHEN** the HTTP connector resolves a request's Host
- **THEN** it SHALL use the unified resolver to derive the tenant and route

### Requirement: Named-Service Projection Boundary
External projection SHALL apply only to named services (leaf aliases). Resource identity URIs such as `sel://<tenant>/region/<id>` SHALL remain internal-only and SHALL NOT project to external wire names.

#### Scenario: Resource id URI stays internal
- **WHEN** a URI contains a typed resource segment (for example `region/42`)
- **THEN** no external wire name SHALL be derived from it

### Requirement: Apex Root-Service Alias
A tenant MAY designate one path as its root service. The bare domain (apex) SHALL then resolve to that path, enabling a domain to map directly to a service. The root service SHALL own the domain's paths: requests for the apex with a request path route to the root service, which handles the path itself.

#### Scenario: Domain maps directly to a service
- **WHEN** the tenant `acme` owns `example.com` and designates `["http", "prod"]` as its root service
- **THEN** resolving `example.com` SHALL route to `sel://acme/http/prod`

#### Scenario: Apex paths route to the root service
- **WHEN** a request arrives for the bare domain with a request path (for example `example.com/healthz`) and the tenant designates a root service
- **THEN** the request SHALL route to the root service, which handles the request path itself

### Requirement: Single Serve Declaration
Registering a named route SHALL derive both its internal path and its wire names from one declaration, and the registering guest SHALL create its own resource rather than receiving a runtime-provisioned one. Resolution SHALL return the registering guest's own target (preserving its interface metadata, e.g. streamed-HTTP markers the connector dispatches on), not a bare pointer to the underlying typed resource.

#### Scenario: One declaration yields both names
- **WHEN** a guest calls `serve` with path `["bridge"]` and a self-created listener
- **THEN** the route SHALL be resolvable as `sel://acme/bridge`, `bridge.acme`, and `bridge.<owned-domain>`

#### Scenario: Resolution returns the registered route target
- **WHEN** a route registered via `serve` (single- or multi-segment, including the app-guest `bind` helpers) is resolved
- **THEN** the resolved target SHALL be the registering guest's own target with its interface metadata intact

### Requirement: Process-Keyed Revocation
Route registrations SHALL be revoked when their owning process exits, keyed by the process recorded in discovery, without a runtime-maintained side map.

#### Scenario: Guest exit revokes its routes
- **WHEN** a guest that registered routes exits or is killed
- **THEN** its registrations SHALL be revoked and cease resolving

### Requirement: Registration-Gated Readiness
A system guest whose descriptor declares a serving role SHALL NOT be admitted as ready until its registration for that role is observable in discovery. `mark_ready()` SHALL remain a bare signal carrying no payload; the runtime SHALL verify registration by querying discovery rather than by reading a payload from the guest.

#### Scenario: System guest ready only after registering
- **WHEN** a system guest declares a serving role and signals readiness before registering it
- **THEN** the runtime SHALL NOT admit the guest as ready and SHALL treat it as failing its readiness condition until the registration is observable in discovery

#### Scenario: Readiness verified via discovery, not a payload
- **WHEN** the runtime gates the readiness of a role-declared guest
- **THEN** it SHALL observe the guest's owner-keyed registrations in discovery rather than depend on any data the guest passes back at readiness
