# Spec Delta

## ADDED Requirements

### Requirement: Malformed HTTP Framing Is Rejected
The HTTP edge SHALL distinguish incomplete, malformed, and oversized input. Invalid, conflicting, or overflowing message-length framing SHALL be rejected and SHALL NOT be interpreted as an empty body or another request.

#### Scenario: Invalid Content-Length
- **WHEN** a request carries a non-numeric, negative, conflicting, or overflowing `Content-Length`
- **THEN** the connector SHALL reject the request with a client-framing error and SHALL NOT forward it or reinterpret remaining bytes

#### Scenario: Chunked trailers terminate correctly
- **WHEN** a chunked body contains multiple trailer fields before its terminating empty line
- **THEN** the connector SHALL consume the complete trailer section and preserve the following request boundary

### Requirement: Route Replacement Converges Connector Caches
A cached route SHALL be refreshed or evicted when discovery reports replacement or revocation, without requiring a failed attachment to trigger convergence.

#### Scenario: Live route is replaced
- **WHEN** a connector holds a cached target and discovery publishes a replacement for that route
- **THEN** a subsequent request SHALL resolve the replacement within the route-watch convergence bound
