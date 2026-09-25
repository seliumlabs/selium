## MODIFIED Requirements

### Requirement: Per-Tenant Bridge Server
The system SHALL support a `bridge-server` system guest deployed one per tenant. The bridge-server SHALL register a `sel://<tenant>/bridge` serving route (a leaf alias under its own tenant) so the QUIC connector delivers bridge-bound connections to it as per-stream handoffs. The bridge-server SHALL NOT terminate QUIC itself and SHALL NOT relay stream bytes.

#### Scenario: Bridge traffic routed to the tenant's bridge-server
- **WHEN** an external client presents an SNI matching a tenant's registered bridge route
- **THEN** the connector SHALL deliver the connection's streams to that tenant's bridge-server

#### Scenario: Bridge-server holds no data plane
- **WHEN** a bridge-server receives a stream handoff
- **THEN** it SHALL forward the handoff to a bridge-channel process rather than relaying bytes itself
