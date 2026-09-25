## MODIFIED Requirements

### Requirement: SNI-Based Discovery Route Resolution
The connector SHALL resolve the serving guest for a connection from the QUIC handshake's server name indication (SNI), matching the normalised name (lowercased, trailing dot stripped) against bare external names registered with discovery. The connector SHALL NOT hold a static routing table. The connector SHALL normalise the SNI identically when resolving and when evicting a cached route, so a stale route cannot survive eviction under a differently-spelled SNI.

#### Scenario: Connection routed to registered guest
- **WHEN** a connection presents an SNI whose normalised form matches a registered bare external name
- **THEN** the connector SHALL forward every stream on that connection to the resolved guest

#### Scenario: Unknown SNI refused at the handshake
- **WHEN** no registration matches the presented SNI (or SNI is absent)
- **THEN** the connector SHALL refuse the connection at the handshake and SHALL NOT contact any app guest

#### Scenario: Cache eviction normalises the raw SNI
- **WHEN** a route cached under a normalised name is evicted using a raw (mixed-case or trailing-dot) spelling of the same SNI
- **THEN** the cached entry SHALL be removed, so the next connection re-resolves

### Requirement: Zero-Network-Grant App Guests
App guests served by the connector SHALL require no `Network` capability grants — only channel attach grants scoped to their per-stream regions (recommended: `ExplicitResource` per stream). Broad shared-memory `UriPrefix` grants SHALL be documented as an anti-pattern for connector-served channels.

#### Scenario: App guest serves with no Network grant
- **WHEN** an app guest holding only channel attach grants is registered for a bare external name
- **THEN** it SHALL receive and answer relayed byte streams successfully

#### Scenario: Ungranted third party cannot intercept
- **WHEN** a guest without a grant for a stream region attempts to attach to it
- **THEN** the runtime SHALL deny the attach
