## ADDED Requirements

### Requirement: Scoped Grant Delegation
A process holding a `Capability::DelegateGrants` grant with a tenant selector SHALL be permitted to spawn child processes whose grants are not a subset of its own, provided **every child grant carries a tenant selector within the parent's delegation scope** and every child grant passes well-formedness admission. Grants without an in-scope tenant selector are unrestricted within their capability and SHALL NOT be conferable under delegation; such spawns fall through to the subset rule. Processes without a matching `DelegateGrants` grant SHALL remain bound by the rule that a child's grants must be a subset of the parent's own grants.

#### Scenario: Delegator spawns within tenant
- **WHEN** a process holding `DelegateGrants` scoped to tenant "acme" spawns a child in tenant "acme" carrying tenant-scoped grants the parent does not itself hold
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

### Requirement: DelegateGrants Is Not Conferable
`DelegateGrants` SHALL be provisioned only at process bootstrap by the host. A spawn that confers `DelegateGrants` on a child SHALL be denied, regardless of whether the parent holds the capability itself or a delegation scope would otherwise admit it, so the exception to authority monotonicity cannot chain through spawned processes.

#### Scenario: Re-delegation denied
- **WHEN** a process holding `DelegateGrants` scoped to its tenant spawns a child whose grants include `DelegateGrants`
- **THEN** the spawn SHALL be denied with a capability error
