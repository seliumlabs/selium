# Spec Delta

## MODIFIED Requirements

### Requirement: Path-Label Reversal

A named service's wire name SHALL be its internal path segments reversed, joined with dots, appended to the tenant's domain. The synthetic domain (`<tenant>`) SHALL always be resolvable cluster-internally; a registered domain SHALL be used when the tenant owns one. A root-namespace (platform-tenant) route SHALL project to the bare reversed path with no tenant domain suffix.

#### Scenario: Bridge route projects to a wire name

- **WHEN** the tenant `acme` serves the path `["bridge"]`
- **THEN** the synthetic wire name `bridge.acme` SHALL resolve to it, and, when `acme` owns `example.com`, `bridge.example.com` SHALL resolve to it

#### Scenario: Deeper path reverses in full

- **WHEN** the tenant `acme` serves the path `["http", "prod"]`
- **THEN** the wire name SHALL be `prod.http.acme` (synthetic) or `prod.http.example.com` (registered)

#### Scenario: Root route projects to a bare name

- **WHEN** a root-namespace guest serves the path `["control"]`
- **THEN** the wire name SHALL be the bare label `control` with no tenant domain suffix, resolving to the internal path `sel:///control`

### Requirement: Advisory Domain-to-Tenant Table

The system SHALL maintain a domain-to-tenant mapping (`example.com -> acme`) provisioned out-of-band. The mapping SHALL be used for routing and for scoping which tenant may register names under a domain. The mapping SHALL NOT be an authentication source.

#### Scenario: Provisioned domain routes to its tenant

- **WHEN** a wire name ends in a registered domain
- **THEN** the resolver SHALL derive the tenant from the mapping and resolve the reversed labels within that tenant's namespace

#### Scenario: Mapping is advisory, not authoritative for identity

- **WHEN** a client connects using a wire name under a domain
- **THEN** the client's identity SHALL still be established by its certificate, and the client's tenant SHALL be derived from that certificate, not from the wire name dialled

#### Scenario: Synthetic tenant label always resolves

- **WHEN** a wire name uses the synthetic `<tenant>` label
- **THEN** the resolver SHALL resolve it without any provisioned domain entry
