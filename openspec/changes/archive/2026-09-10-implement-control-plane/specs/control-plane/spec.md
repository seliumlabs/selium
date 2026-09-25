## Purpose

`selium-control-plane` is the per-tenant desired-state layer: a system guest that serves a typed control RPC surface to externally authenticated clients over the bridge, owns user-facing desired state, and delegates platform policy to the scheduler, supervisor, and discovery system guests.

## ADDED Requirements

### Requirement: Control-Plane Guest Crate
The system SHALL provide a `selium-control-plane` guest crate that builds as a `wasm32-unknown-unknown` system guest and bootstraps through `selium-runtime` configuration.

#### Scenario: Control-plane bootstraps from descriptor
- **WHEN** `selium-runtime` receives a `SystemGuestDescriptor` for `selium-control-plane`
- **THEN** it SHALL start the control-plane guest using the descriptor's module, zero-argument entrypoint, grants, dependencies, and readiness condition

### Requirement: Served Typed Control Surface
The control-plane guest SHALL create a host-queue listener and register it as a named service — internal path `sel://<tenant>/control`, external wire name `control.<tenant>` — through the discovery serving surface. External clients SHALL reach the served surface through the bridge: for host-queue targets the bridge SHALL perform the RPC rendezvous on the client's behalf.

#### Scenario: Serving route registered
- **WHEN** the control-plane entrypoint runs
- **THEN** it SHALL create its own listener, register the `control.<tenant>` serving route, and report readiness only after the route is registered

#### Scenario: External client reaches the served surface
- **WHEN** an authenticated client opens the `control.<tenant>` route through the bridge
- **THEN** the bridge SHALL establish an RPC session on the client's behalf — allocating a session region, splicing the client's stream into it, and enqueuing the session into the control-plane's served host queue
- **AND** the control-plane SHALL accept the session and serve typed request/reply over it

### Requirement: Typed Wire Protocol
The control-plane request and reply types SHALL be FlatBuffers-encoded messages exchanged over typed RPC. The control surface SHALL NOT define a text protocol for control requests.

#### Scenario: Typed request and reply
- **WHEN** a client sends a valid encoded control request on an accepted session
- **THEN** the control-plane SHALL decode it and reply with a typed response over the same channel

#### Scenario: No text grammar for control
- **WHEN** a control request cannot be decoded as the typed request message
- **THEN** the control-plane SHALL return a typed serialization error rather than interpreting a text command

### Requirement: User-Facing Desired State Ownership
The control-plane SHALL own the user-facing desired state for workloads: deployments and pipeline bindings. Accepting an update SHALL record it in control-plane-owned durable state and SHALL make it visible through a live-table state projection.

#### Scenario: Deployment update recorded
- **WHEN** a deployment update is accepted for a workload
- **THEN** the control-plane SHALL append the desired state to its durable log and apply it to its live-table projection

#### Scenario: Desired state read reflects accepted updates
- **WHEN** a reader queries the deployment projection
- **THEN** it SHALL observe the last accepted desired state for that workload

#### Scenario: Pipeline binding recorded
- **WHEN** a pipeline binding between workload endpoints is accepted
- **THEN** it SHALL be recorded in control-plane-owned desired state

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
Access to the served control surface SHALL be enforced by the capability system. A client whose grants do not cover the control-plane resource class SHALL be refused, and the control-plane SHALL NOT implement an ad-hoc identity check as a substitute.

#### Scenario: Privileged client reaches the control surface
- **WHEN** a client whose grants include the control-plane capability opens the serving route
- **THEN** the control-plane SHALL serve the session

#### Scenario: Unprivileged client is refused
- **WHEN** a client without a control-plane grant attempts to reach the served surface
- **THEN** capability enforcement SHALL refuse the attach
- **AND** the control-plane SHALL NOT rely on its own identity parsing to admit the session

### Requirement: Module Upload via Storage Hostcalls
The control-plane SHALL store uploaded module bytes through the storage hostcall surface (`StorageBlobPut`) and SHALL record a manifest name for the stored module. Deployment intents SHALL reference stored modules by their blob identity or manifest name.

#### Scenario: Upload stores a module
- **WHEN** a client uploads module bytes
- **THEN** the control-plane SHALL write them to the blob store and return a manifest name

#### Scenario: Deploy references a stored module
- **WHEN** a deployment references an uploaded module
- **THEN** the recorded deployment SHALL refer to the module's blob identity or manifest name

### Requirement: Delegation Error Propagation
When a delegated interaction fails, the control-plane SHALL return a typed error identifying the failed step and its context to the external caller.

#### Scenario: Delegated failure carries step and context
- **WHEN** a delegated scheduler or discovery interaction returns an error
- **THEN** the control-plane SHALL reply with a typed failure naming the step that failed and the relevant context
