# Spec Delta

## ADDED Requirements

### Requirement: Route Replacement Converges Connector Caches
A cached QUIC serving route SHALL follow discovery replacement and revocation events; attachment failure SHALL remain an additional eviction trigger, not the only convergence mechanism.

#### Scenario: Live route is replaced
- **WHEN** a connector holds a cached target and discovery publishes a replacement for the same SNI-derived route
- **THEN** a subsequent connection SHALL use the replacement within the route-watch convergence bound

#### Scenario: Cached route is revoked
- **WHEN** discovery revokes a cached route
- **THEN** the connector SHALL stop using it and apply the unknown-route behaviour to subsequent connections
