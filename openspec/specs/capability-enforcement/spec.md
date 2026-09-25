# Spec: Capability Enforcement

## Purpose

Define the capability enforcement model for Selium hostcall authorisation: scope-context evaluation against real authority values, unguessable resource identities, ownership-checked region attach, descendant-scoped telemetry reads, and failure-cleanup that preserves co-owners of shared resources.

## Requirements

### Requirement: Evaluatable Scope Contexts

Every hostcall authorisation decision SHALL be evaluated against a
`ScopeContext` populated from the calling process's authority: tenant,
resource URI (where the runtime knows it), locality, resource class, and
resource identity. Selectors SHALL be evaluated against real values, not
placeholder `None`s.

#### Scenario: Tenant isolation enforces

- **WHEN** two processes with different tenants invoke the same hostcall
  class, one holding a `Tenant`-scoped grant for its own tenant
- **THEN** the matching process succeeds and the other is denied with an
  error naming the denied capability and tenant

#### Scenario: Selector admission at grant time

- **WHEN** a grant containing a selector the runtime cannot evaluate
  (for example `UriPrefix` without a network `ResourceClass` selector)
  is presented at spawn or `ProcessStart`
- **THEN** it is rejected with an error naming the selector, before any
  hostcall is attempted

### Requirement: Unguessable Resource Identities

Shared and local resource ids SHALL be allocated non-sequentially such
that knowing one id confers negligible advantage in guessing another.

#### Scenario: Guessing is not viable

- **WHEN** a process attempts `AttachRegion` on an id it was never
  granted or assigned
- **THEN** the attempt fails ownership validation regardless of the id's
  numeric proximity to ids it does own

### Requirement: Ownership-Checked Attach

`AttachRegion` SHALL succeed only when the caller owns the region, holds
an `ExplicitResource` grant for it, or received it through the documented
host-queue handoff. No other implicit sharing SHALL exist.

#### Scenario: Queue handoff shares ownership

- **WHEN** a process receives a region id via a host queue it is attached
  to
- **THEN** ownership is shared with the receiver at receive time and the
  subsequent `AttachRegion` succeeds

#### Scenario: No implicit sharing

- **WHEN** a process with a class-level `SharedMemory` grant attempts to
  attach to a region it does not own
- **THEN** the attempt is denied

### Requirement: Descendant Telemetry Reads

Metering, activity, and guest-log reads SHALL accept an
`ExplicitResource(Local(pid))` grant or a descendant-scope selector
matching processes spawned by the grantee. Log writes SHALL treat the
writer's own pid as owned.

#### Scenario: Supervisor reads child telemetry

- **WHEN** a supervisor with a descendant-scope grant reads metering for
  a process it spawned (directly or transitively)
- **THEN** the read succeeds; reads of unrelated processes are denied

### Requirement: Cleanup Preserves Co-owners

Failure-cleanup of one process SHALL NOT remove other processes from the
owner sets of shared resources.

#### Scenario: Co-owner survives cleanup

- **WHEN** two processes co-own a region and one fails
- **THEN** the surviving process retains ownership and can still attach
  and use the region

### Requirement: Network Endpoint URIs in Scope Contexts

The runtime SHALL populate `ScopeContext.uri` with a canonical network
endpoint URI (`tcp://<host>:<port>` or `udp://<host>:<port>`) when
evaluating `TcpBind`, `TcpConnect`, and `UdpBind` hostcalls.
Canonicalisation SHALL lowercase the host, strip any trailing dot,
bracket IPv6 literals, and always include the port explicitly.

#### Scenario: Connect evaluated against URI grant
- **WHEN** a guest issues `TcpConnect` to `93.184.216.34:443`
- **THEN** the runtime SHALL evaluate grants against
  `uri = "tcp://93.184.216.34:443"` with
  `resource_class = TcpStream`

#### Scenario: Denial names the URI
- **WHEN** no grant admits the requested endpoint
- **THEN** the hostcall SHALL fail with `PermissionDenied` and the error
  message SHALL include the canonical URI

### Requirement: Component-Aware URI Prefix Matching

When a `ResourceSelector::UriPrefix` grant and the context URI both parse
as network endpoints, matching SHALL compare components: scheme exact;
host exact or `*.`-label-boundary wildcard; port exact, list, or `*`.
Plain string prefix semantics SHALL remain for non-network URIs.

#### Scenario: Label-suffix attack rejected
- **WHEN** a grant carries `UriPrefix("tcp://93.184.216.34:443")`
- **THEN** a context URI of `tcp://93.184.216.34:443` matches, and a
  context URI whose host merely has the grant host as a string prefix
  (e.g. a longer differing literal) SHALL NOT match

#### Scenario: Port wildcard
- **WHEN** a grant carries `UriPrefix("tcp://127.0.0.1:*")`
- **THEN** any loopback port matches and any non-loopback host SHALL NOT
  match

### Requirement: Grant-Time Evaluatable Honesty for UriPrefix

A `CapabilityGrant` containing `ResourceSelector::UriPrefix` SHALL be
accepted at grant-registration time only if the same grant's selectors
include `ResourceClass::TcpListener`, `ResourceClass::TcpStream`, or
`ResourceClass::UdpSocket`. Otherwise registration SHALL fail loudly.

#### Scenario: UriPrefix on non-network class rejected
- **WHEN** a grant is registered with selectors
  `[ResourceClass(DurableLog), UriPrefix("tcp://10.0.0.5:443")]`
- **THEN** registration SHALL fail with an error explaining that
  `UriPrefix` is not evaluatable for that class

### Requirement: Namespace Selector Hierarchy

`ResourceSelector::Namespace(namespace)` SHALL be evaluated hierarchically: `Namespace::Tenant(t)` matches a scope context whose tenant is exactly `t`, and `Namespace::Root` matches a scope context in the root namespace. Where a capability confers authority that spans tenants (for example `DelegateGrants`), a `Namespace::Root`-scoped grant SHALL admit operation on any tenant: the root namespace is greater than any tenant.

#### Scenario: Tenant selector admits only its tenant

- **WHEN** a grant carries `Namespace::Tenant("acme")` and the scope context's tenant is `acme`
- **THEN** the selector matches
- **AND** the same selector evaluated against a scope context with tenant `beta` or with no tenant SHALL NOT match

#### Scenario: Root selector admits the root namespace

- **WHEN** a grant carries `Namespace::Root` and the scope context has no tenant (root namespace)
- **THEN** the selector matches

### Requirement: Scoped Grant Delegation

A process holding a `Capability::DelegateGrants` grant carrying a tenant
or root namespace selector SHALL be permitted to spawn child processes
whose grants are not a subset of its own, provided every child grant
carries a selector within the parent's delegation scope and every child
grant passes well-formedness admission. A `DelegateGrants` grant carrying
`Namespace::Root` SHALL admit child grants scoped to any tenant. A
`DelegateGrants` grant carrying a tenant selector SHALL admit child grants
scoped only to that tenant. Grants without an in-scope tenant selector are
unrestricted within their capability and SHALL NOT be conferable under
delegation; such spawns fall through to the subset rule. Processes without
a matching `DelegateGrants` grant SHALL remain bound by the rule that a
child's grants must be a subset of the parent's own grants.

#### Scenario: Delegator spawns within tenant

- **WHEN** a process holding `DelegateGrants` scoped to tenant "acme"
  spawns a child in tenant "acme" carrying tenant-scoped grants the parent
  does not itself hold
- **THEN** the spawn SHALL succeed, provided each child grant passes
  admission

#### Scenario: Root delegator spawns for any tenant

- **WHEN** a process holding `DelegateGrants` scoped to `Namespace::Root`
  spawns a child in any tenant carrying tenant-scoped grants the parent
  does not itself hold
- **THEN** the spawn SHALL succeed, provided each child grant passes
  admission

#### Scenario: Non-delegator out-of-scope child denied

- **WHEN** a process without a matching `DelegateGrants` grant spawns a
  child with grants exceeding its own
- **THEN** the spawn SHALL be denied with a capability error

#### Scenario: Delegation outside tenant denied

- **WHEN** a process holding `DelegateGrants` scoped to tenant "acme"
  spawns a child in tenant "beta"
- **THEN** the spawn SHALL be denied with a capability error naming the
  tenant

#### Scenario: Unscoped grant not conferable under delegation

- **WHEN** a process holding `DelegateGrants` scoped to tenant "acme"
  spawns a child whose grant set mixes a tenant-scoped grant with an
  unrestricted (selector-less) grant
- **THEN** the spawn SHALL be denied with a capability error

### Requirement: DelegateGrants Is Not Conferable

`DelegateGrants` SHALL be provisioned only at process bootstrap by the
host. A spawn that confers `DelegateGrants` on a child SHALL be denied,
regardless of whether the parent holds the capability itself or a
delegation scope would otherwise admit it, so the exception to authority
monotonicity cannot chain through spawned processes.

#### Scenario: Re-delegation denied

- **WHEN** a process holding `DelegateGrants` scoped to its tenant spawns
  a child whose grants include `DelegateGrants`
- **THEN** the spawn SHALL be denied with a capability error

### Requirement: MintCertificate Is Not Conferable

`MintCertificate` SHALL be provisioned only at process bootstrap by the host. A spawn that confers `MintCertificate` on a child SHALL be denied, regardless of whether the parent holds the capability itself or a delegation scope would otherwise admit it.

#### Scenario: Providing MintCertificate at spawn is denied

- **WHEN** a process holding `MintCertificate` spawns a child whose grants include `MintCertificate`
- **THEN** the spawn SHALL be denied with a capability error

### Requirement: QuotaWrite Is Not Conferable

`QuotaWrite` SHALL be provisioned only at process bootstrap by the host. A spawn that confers `QuotaWrite` on a child SHALL be denied, regardless of whether the parent holds the capability itself or a delegation scope would otherwise admit it.

#### Scenario: Conferring QuotaWrite at spawn is denied

- **WHEN** a process holding `QuotaWrite` spawns a child whose grants include `QuotaWrite`
- **THEN** the spawn SHALL be denied with a capability error
