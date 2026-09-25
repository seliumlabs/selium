# Spec Delta

## ADDED Requirements

### Requirement: Namespace Selector Hierarchy

`ResourceSelector::Namespace(namespace)` SHALL be evaluated hierarchically: `Namespace::Tenant(t)` matches a scope context whose tenant is exactly `t`, and `Namespace::Root` matches a scope context in the root namespace. Where a capability confers authority that spans tenants (for example `DelegateGrants`), a `Namespace::Root`-scoped grant SHALL admit operation on any tenant: the root namespace is greater than any tenant.

#### Scenario: Tenant selector admits only its tenant

- **WHEN** a grant carries `Namespace::Tenant("acme")` and the scope context's tenant is `acme`
- **THEN** the selector matches
- **AND** the same selector evaluated against a scope context with tenant `beta` or with no tenant SHALL NOT match

#### Scenario: Root selector admits the root namespace

- **WHEN** a grant carries `Namespace::Root` and the scope context has no tenant (root namespace)
- **THEN** the selector matches

## MODIFIED Requirements

### Requirement: Scoped Grant Delegation

A process holding a `Capability::DelegateGrants` grant carrying a tenant or root namespace selector SHALL be permitted to spawn child processes whose grants are not a subset of its own, provided every child grant carries a selector within the parent's delegation scope and every child grant passes well-formedness admission. A `DelegateGrants` grant carrying `Namespace::Root` SHALL admit child grants scoped to any tenant. A `DelegateGrants` grant carrying a tenant selector SHALL admit child grants scoped only to that tenant. Grants without an in-scope tenant selector are unrestricted within their capability and SHALL NOT be conferable under delegation; such spawns fall through to the subset rule. Processes without a matching `DelegateGrants` grant SHALL remain bound by the rule that a child's grants must be a subset of the parent's own grants.

#### Scenario: Delegator spawns within tenant

- **WHEN** a process holding `DelegateGrants` scoped to tenant "acme" spawns a child in tenant "acme" carrying tenant-scoped grants the parent does not itself hold
- **THEN** the spawn SHALL succeed, provided each child grant passes admission

#### Scenario: Root delegator spawns for any tenant

- **WHEN** a process holding `DelegateGrants` scoped to `Namespace::Root` spawns a child in any tenant carrying tenant-scoped grants the parent does not itself hold
- **THEN** the spawn SHALL succeed, provided each child grant passes admission

#### Scenario: Non-delegator out-of-scope child denied

- **WHEN** a process without a matching `DelegateGrants` grant spawns a child with grants exceeding its own
- **THEN** the spawn SHALL be denied with a capability error

#### Scenario: Delegation outside tenant denied

- **WHEN** a process holding `DelegateGrants` scoped to tenant "acme" spawns a child in tenant "beta"
- **THEN** the spawn SHALL be denied with a capability error naming the tenant

#### Scenario: Unscoped grant not conferable under delegation

- **WHEN** a process holding `DelegateGrants` scoped to tenant "acme" spawns a child whose grant set mixes a tenant-scoped grant with an unrestricted (selector-less) grant
- **THEN** the spawn SHALL be denied with a capability error
