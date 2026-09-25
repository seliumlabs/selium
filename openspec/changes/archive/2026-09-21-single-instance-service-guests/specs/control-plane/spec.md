# Spec Delta

## MODIFIED Requirements

### Requirement: Served Typed Control Surface

The control-plane guest SHALL be a single per-platform instance. It SHALL create a host-queue listener and register it as a named root-namespace service — internal path `sel:///control`, external wire name `control` — through the discovery serving surface. Sessions SHALL be delivered to the served host queue as internal shared-memory RPC rendezvous (the same path the discovery guest serves); an external client SHALL reach the control surface through a bridge-channel spawned under its authenticated tenant, which rendezvouses the session into the served queue. The control-plane SHALL accept each session and serve typed request/reply over it, scoped to the delivering process's tenant.

#### Scenario: Serving route registered

- **WHEN** the control-plane entrypoint runs
- **THEN** it SHALL create its own listener, register the `control` serving route in the root namespace, and report readiness only after the route is registered

#### Scenario: External client reaches the served surface

- **WHEN** an authenticated client is bridged to the control surface (a bridge-channel scoped to that client's tenant delivers a session into the served queue)
- **THEN** the control-plane SHALL accept the session and serve typed request/reply over it, scoped to the delivering bridge-channel's tenant

### Requirement: Capability-Enforced Access

Access to the served control surface SHALL be enforced by the capability system at attach time: the runtime SHALL gate attach to the serving queue by the delivering process's grants. The control-plane SHALL derive its per-session tenant only from the delivering process's tenant — the runtime-persisted process authority, never a tenant the caller asserts for itself — and SHALL refuse any session whose process tenant cannot be resolved.

#### Scenario: Privileged client reaches the control surface

- **WHEN** a client whose grants otherwise admit the control-plane exchange reaches the serving route
- **THEN** the control-plane SHALL derive the session's tenant from the delivering process's tenant and serve it

#### Scenario: Unprivileged client is refused

- **WHEN** a client without a grant that admits the control-plane exchange attempts to reach the served surface
- **THEN** capability enforcement SHALL refuse the attach
- **AND** the control-plane SHALL NOT serve the session

#### Scenario: Session with unresolvable tenant refused

- **WHEN** a session's delivering process tenant cannot be resolved by the runtime
- **THEN** the control-plane SHALL refuse the session without serving it

### Requirement: User-Facing Desired State Ownership

The control-plane SHALL own the user-facing desired state for workloads: deployments and pipeline bindings. Desired state SHALL be recorded in a single platform-scoped durable log whose records carry the tenant they belong to, and SHALL be projected into a read model partitioned by tenant. Reads in a session SHALL observe only the session's own tenant partition.

#### Scenario: Deployment update recorded

- **WHEN** a deployment update is accepted in a session scoped to a tenant
- **THEN** the control-plane SHALL append a desired-state record carrying that tenant to the platform durable log and apply it to that tenant's partition of the projection

#### Scenario: Desired state read reflects accepted updates

- **WHEN** a reader queries the deployment projection within a tenant-scoped session
- **THEN** it SHALL observe the last accepted desired state for that workload within that tenant's partition only

#### Scenario: Pipeline binding recorded

- **WHEN** a pipeline binding between workload endpoints is accepted in a tenant-scoped session
- **THEN** it SHALL be recorded in that tenant's desired-state partition

### Requirement: Module Upload via Storage Hostcalls

The control-plane SHALL store uploaded module bytes through the storage hostcall surface (`StorageBlobPut`) and SHALL record a tenant-prefixed manifest name for the stored module. Deployment intents SHALL reference stored modules by their blob identity or tenant-prefixed manifest name.

#### Scenario: Upload stores a module

- **WHEN** a client uploads module bytes in a tenant-scoped session
- **THEN** the control-plane SHALL write them to the blob store and return a manifest name prefixed with the session's tenant

#### Scenario: Deploy references a stored module

- **WHEN** a deployment references an uploaded module
- **THEN** the recorded deployment SHALL refer to the module's blob identity or tenant-prefixed manifest name

## ADDED Requirements

### Requirement: Requestor Tenant Derivation

The control-plane SHALL derive the requestor's tenant for each session from the delivering process's tenant (its process owner). A named process tenant SHALL scope the session to that tenant's namespace; an unset (root) process tenant SHALL scope the session to the root namespace. The control-plane SHALL NOT derive a session's tenant from handoff metadata or any other tenant the caller asserts for itself.

#### Scenario: Tenant-scoped deliverer scoped to its tenant

- **WHEN** a session is delivered by a process whose tenant is `acme`
- **THEN** the session SHALL be scoped to the tenant namespace `acme`

#### Scenario: Root deliverer scoped as root

- **WHEN** a session is delivered by a process with a root (unset) process tenant
- **THEN** the session SHALL be scoped to the root namespace

#### Scenario: External client scoped through its bridge-channel

- **WHEN** an external client is bridged to the control surface by a bridge-channel spawned under the client's authenticated tenant `acme`
- **THEN** the session's delivering process (the bridge-channel) SHALL carry tenant `acme`, so the session SHALL be scoped to the tenant namespace `acme`
