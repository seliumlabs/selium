## MODIFIED Requirements

### Requirement: SNI-Based Discovery Route Resolution
The connector SHALL resolve the serving guest for a connection from the QUIC handshake's server name indication (SNI) through the unified addressing resolver: derive the tenant from the domain-to-tenant table (or the synthetic tenant label), reverse the remaining labels into a path, and resolve that path via discovery. The connector SHALL NOT hold a static routing table. The connector SHALL normalise the SNI identically when resolving and when evicting a cached route, so a stale route cannot survive eviction under a differently-spelled SNI.

#### Scenario: Connection routed to registered guest
- **WHEN** a connection presents an SNI that derives a tenant and reverses to a registered path
- **THEN** the connector SHALL forward every stream on that connection to the resolved guest

#### Scenario: Registered domain SNI
- **WHEN** a connection presents SNI `bridge.example.com` and `example.com -> acme` is provisioned
- **THEN** the connector SHALL resolve to the same `acme` bridge route it resolves for `bridge.acme`

#### Scenario: Unknown SNI refused at the handshake
- **WHEN** the SNI derives no tenant, resolves to no route, or is absent
- **THEN** the connector SHALL refuse the connection at the handshake and SHALL NOT contact any app guest

#### Scenario: Cache eviction normalises the raw SNI
- **WHEN** a route cached under a normalised name is evicted using a raw (mixed-case or trailing-dot) spelling of the same SNI
- **THEN** the cached entry SHALL be removed, so the next connection re-resolves
