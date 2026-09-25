# Spec: Capability Enforcement

## ADDED Requirements

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
  (currently `UriPrefix`) is presented at spawn or `ProcessStart`
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
