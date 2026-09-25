## ADDED Requirements

### Requirement: Per-Connection Bidirectional Stream Cap
The connector SHALL cap the number of concurrent bidirectional streams a connection may open, by configuring the QUIC endpoint's per-connection bidirectional stream limit, so a single connection cannot fan out into an unbounded number of per-stream channels. Streams beyond the cap SHALL NOT be accepted and SHALL NOT cause a per-stream region allocation.

#### Scenario: Connection exceeds the stream cap
- **WHEN** a client opens more concurrent bidirectional streams than the configured per-connection cap
- **THEN** the connector SHALL refuse the excess streams before allocating a per-stream channel

### Requirement: Per-Tenant Stream Admission Rate Limit
The connector SHALL rate-limit new stream admissions per tenant using a token bucket with a deployable-default rate and burst (the exact values are operator configuration, not spec behaviour), keyed by the authenticated client's tenant, falling back to the resolved serving tenant when client authentication is disabled. When the bucket is exhausted, the connector SHALL refuse the stream cheaply — resetting the stream with a distinct error code while keeping the connection — before allocating a per-stream channel, so a stream flood costs the least possible before reaching a serving guest.

#### Scenario: Flood above the rate is refused cheaply
- **WHEN** a tenant opens streams faster than the admission rate and the bucket is empty
- **THEN** the connector SHALL refuse the excess streams before allocating their channels

#### Scenario: Unauthenticated endpoint falls back to the serving tenant
- **WHEN** client authentication is disabled and a client opens streams over the admission rate
- **THEN** the connector SHALL rate-limit against the resolved serving tenant's bucket
