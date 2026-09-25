# Spec Delta

## MODIFIED Requirements

### Requirement: Per-Tenant Bridge Server

The system SHALL support a single per-platform `bridge-server` system guest serving every tenant. The bridge-server SHALL register a `sel:///bridge` serving route (a root-namespace leaf alias) so the QUIC connector delivers bridge-bound connections to it as per-stream handoffs, and SHALL derive each stream's tenant from the handoff's authenticated identity. The bridge-server SHALL NOT terminate QUIC itself and SHALL NOT relay stream bytes.

#### Scenario: Bridge traffic routed to the tenant's bridge-server

- **WHEN** an external client presents an SNI resolving to the `bridge` serving route
- **THEN** the connector SHALL deliver the connection's streams to the single bridge-server

#### Scenario: Bridge-server holds no data plane

- **WHEN** a bridge-server receives a stream handoff
- **THEN** it SHALL forward the handoff to a bridge-channel process rather than relaying bytes itself

### Requirement: Identity to Grant Resolution

The bridge-server SHALL resolve a handoff's authenticated TLS identity — tenant scope and key fingerprint — to a capability grant set before spawning. The tenant scope SHALL be taken from the identity itself, and the bridge-server SHALL confer and narrow grants for whichever tenant the identity carries; the bridge-server has no own tenant to restrict the identity to. When the identity is unknown, the bridge-server SHALL close the delivered stream and SHALL NOT spawn a bridge-channel.

#### Scenario: Known identity resolves grants and spawns

- **WHEN** a handoff carries an identity present in the identity source
- **THEN** the bridge-server SHALL spawn a bridge-channel holding that identity's grants, narrowed for that identity's tenant

#### Scenario: Unknown identity refused

- **WHEN** a handoff carries an identity not present in the identity source
- **THEN** the bridge-server SHALL close the delivered stream without spawning a bridge-channel

#### Scenario: Cross-tenant identity refused

- **WHEN** a handoff carries an authenticated identity for a tenant (for example `acme`)
- **THEN** the bridge-server SHALL NOT refuse it on cross-tenant grounds, because the single bridge-server is bound to no one tenant
- **AND** the bridge-server SHALL confer and narrow grants for the identity's own tenant and spawn a bridge-channel scoped to that tenant

### Requirement: Accountant Narrowing at Conferral

The bridge-server SHALL fold the accountant's published narrowing state for the identity's tenant into the identity-published baseline grants before conferring a grant set on a spawned bridge-channel.

#### Scenario: Baseline narrowed before conferral

- **WHEN** the bridge-server resolves a handoff identity and the accountant has published narrowing state for the identity's tenant
- **THEN** the bridge-channel SHALL be spawned holding the baseline grants minus that tenant's narrowing

#### Scenario: No narrowing is a no-op

- **WHEN** the identity's tenant has no published narrowing
- **THEN** the bridge-channel SHALL receive the identity-published baseline grants unchanged

#### Scenario: Narrowing table unreachable fails open

- **WHEN** the bridge-server boots and the accountant's narrowing table route cannot be resolved or attached (the accountant is down or booting)
- **THEN** the bridge-server SHALL continue conferring identity-published baseline grants un-narrowed (fail-open), with a warning logged — consistent with the accepted v1 single-accountant SPOF; the accountant re-publishes its narrowing state at its own boot, and conferrals fold it in from the first sync after it returns
