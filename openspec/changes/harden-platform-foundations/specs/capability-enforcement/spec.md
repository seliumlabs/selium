# Spec Delta

## ADDED Requirements

### Requirement: Operation-Target Certificate Authorisation
Certificate signing and revocation SHALL evaluate the caller's grant against the tenant named by the operation, not merely the caller's own tenant.

#### Scenario: Tenant-scoped mint cannot cross tenants
- **WHEN** a process holding `MintCertificate` only for tenant `acme` requests signing or revocation for tenant `beta`
- **THEN** the host SHALL deny the operation with `PermissionDenied` and SHALL NOT change beta's key material

#### Scenario: Authorised target succeeds
- **WHEN** a process holds `MintCertificate` authority covering the requested tenant
- **THEN** the host SHALL perform the requested certificate operation for that tenant

### Requirement: Intersection-Preserving Delegation
Delegation scope SHALL be derived per grant while preserving intersection semantics across every selector in that grant; selectors from separate grants SHALL NOT be combined into broader authority.

#### Scenario: Root and tenant selectors remain conjunctive
- **WHEN** one delegation grant carries both `Namespace::Root` and `Tenant("acme")` selectors
- **THEN** it SHALL NOT be treated as unrestricted root delegation and SHALL admit only operations satisfying both selectors

#### Scenario: Separate grants do not merge authority
- **WHEN** one grant carries a root selector and another carries a tenant selector
- **THEN** the runtime SHALL evaluate each grant independently and SHALL NOT synthesise a new combined delegation scope
