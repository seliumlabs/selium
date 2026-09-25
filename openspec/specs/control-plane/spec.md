## Purpose

`selium-control-plane` is the per-tenant desired-state layer: a system guest that serves a typed control RPC surface to externally authenticated clients over the bridge, owns user-facing desired state, and delegates platform policy to the scheduler, supervisor, and discovery system guests.

## Requirements

### Requirement: Control-Plane Guest Crate
The system SHALL provide a `selium-control-plane` guest crate that builds as a `wasm32-unknown-unknown` system guest and bootstraps through `selium-runtime` configuration.

#### Scenario: Control-plane bootstraps from descriptor
- **WHEN** `selium-runtime` receives a `SystemGuestDescriptor` for `selium-control-plane`
- **THEN** it SHALL start the control-plane guest using the descriptor's module, zero-argument entrypoint, grants, dependencies, and readiness condition

### Requirement: Served Typed Control Surface
The control-plane guest SHALL be a single per-platform instance. It SHALL create a host-queue listener and register it as a named root-namespace service — internal path `sel:///control`, external wire name `control` — through the discovery serving surface. Sessions SHALL be delivered to the served host queue as internal shared-memory RPC rendezvous (the same path the discovery guest serves); an external client SHALL reach the control surface through a bridge-channel spawned under its authenticated tenant, which rendezvouses the session into the served queue. The control-plane SHALL accept each session and serve typed request/reply over it, scoped to the delivering process's tenant.

#### Scenario: Serving route registered
- **WHEN** the control-plane entrypoint runs
- **THEN** it SHALL create its own listener, register the `control` serving route in the root namespace, and report readiness only after the route is registered

#### Scenario: External client reaches the served surface
- **WHEN** an authenticated client is bridged to the control surface (a bridge-channel scoped to that client's tenant delivers a session into the served queue)
- **THEN** the control-plane SHALL accept the session and serve typed request/reply over it, scoped to the delivering bridge-channel's tenant

### Requirement: Typed Wire Protocol
The control-plane request and reply types SHALL be FlatBuffers-encoded messages exchanged over typed RPC. The control surface SHALL NOT define a text protocol for control requests.

#### Scenario: Typed request and reply
- **WHEN** a client sends a valid encoded control request on an accepted session
- **THEN** the control-plane SHALL decode it and reply with a typed response over the same channel

#### Scenario: No text grammar for control
- **WHEN** a control request cannot be decoded as the typed request message
- **THEN** the control-plane SHALL return a typed serialization error rather than interpreting a text command

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

### Requirement: Narrow Delegation
The control-plane SHALL interpret external intent and SHALL delegate platform policy: placement decisions to `selium-scheduler`, supervision and recovery policy to `selium-supervisor`, and naming or resolution to `selium-discovery`. The control-plane SHALL NOT make placement, recovery, or discovery-naming decisions itself.

#### Scenario: Placement delegated to scheduler
- **WHEN** a request requires workload placement
- **THEN** the control-plane SHALL send a placement request to the scheduler rather than choosing hosts locally

#### Scenario: Resolution delegated to discovery
- **WHEN** a request requires URI resolution
- **THEN** the control-plane SHALL resolve through the discovery service rather than maintaining its own mapping

#### Scenario: Recovery delegated to supervisor
- **WHEN** a request concerns process recovery or restart policy
- **THEN** the control-plane SHALL relay that intent to the supervisor rather than applying recovery policy locally

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

### Requirement: Module Upload via Storage Hostcalls
The control-plane SHALL store uploaded module bytes through the storage hostcall surface (`StorageBlobPut`) and SHALL record a tenant-prefixed manifest name for the stored module. Deployment intents SHALL reference stored modules by their blob identity or tenant-prefixed manifest name.

#### Scenario: Upload stores a module
- **WHEN** a client uploads module bytes in a tenant-scoped session
- **THEN** the control-plane SHALL write them to the blob store and return a manifest name prefixed with the session's tenant

#### Scenario: Deploy references a stored module
- **WHEN** a deployment references an uploaded module
- **THEN** the recorded deployment SHALL refer to the module's blob identity or tenant-prefixed manifest name

### Requirement: Delegation Error Propagation
When a delegated interaction fails, the control-plane SHALL return a typed error identifying the failed step and its context to the external caller.

#### Scenario: Delegated failure carries step and context
- **WHEN** a delegated scheduler or discovery interaction returns an error
- **THEN** the control-plane SHALL reply with a typed failure naming the step that failed and the relevant context

### Requirement: FlatBuffers Desired-State Log Records
The control plane SHALL encode the desired-state records it appends to its durable log as FlatBuffers via `FlatMsg`, and SHALL decode them with `FlatMsg` when rebuilding the projection from replay. The rkyv codec SHALL NOT be used for desired-state persistence.

#### Scenario: Deployment intent is recorded as FlatBuffers
- **WHEN** a deployment update is accepted
- **THEN** the control plane SHALL append a FlatBuffers-encoded `DesiredStateRecord::Deployment(..)` payload to the durable log

#### Scenario: Projection rebuilds from FlatBuffers replay
- **WHEN** the control plane replays the durable log on boot
- **THEN** it SHALL decode each record payload with `FlatMsg` and apply it to the projection
- **AND** a record that fails FlatBuffers decode SHALL be skipped and logged, never silently reinterpreted

#### Scenario: Desired-state types live in the service crate
- **WHEN** the control plane constructs or matches `DesiredStateRecord`, `Deployment`, or `PipelineBinding`
- **THEN** it SHALL import those types from `selium-service`, not `selium-abi`