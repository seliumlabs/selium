## ADDED Requirements

### Requirement: Scheduler Guest Crate
The system SHALL provide a `selium-scheduler` guest crate that can be built as a `wasm32-unknown-unknown` system guest and bootstrapped through `selium-runtime` configuration.

#### Scenario: Scheduler guest bootstraps from descriptor
- **WHEN** `selium-runtime` receives a `SystemGuestDescriptor` for `selium-scheduler`
- **THEN** it SHALL start the scheduler guest using the descriptor's module, zero-argument entrypoint, grants, dependencies, and readiness condition

### Requirement: Scheduler-Owned Durable State
The scheduler guest SHALL own scheduler placement state and SHALL reconcile local host actions against that state.

#### Scenario: Placement state updated
- **WHEN** a placement intent is accepted by the scheduler
- **THEN** the scheduler SHALL write the resulting desired state into scheduler-owned durable or live state before reconciling host-local actions

#### Scenario: Running scheduler persists desired state
- **WHEN** the scheduler guest entrypoint accepts placement intent through its configured interface
- **THEN** it SHALL persist desired state through a durable log or live table resource before reporting the placement as accepted
- **AND** native-only `SchedulerState` updates SHALL NOT satisfy this requirement

### Requirement: State-Machine Placement Flow
The scheduler guest SHALL operate as a state machine that reads current placement state, computes changes, writes desired state, and reconciles observed host state.

#### Scenario: Placement request handled
- **WHEN** the scheduler receives a placement intent through its guest-facing interface
- **THEN** it SHALL read the current scheduling inputs, compute a placement decision, publish the desired state, and reconcile toward that state

### Requirement: Placement Inputs
The scheduler guest SHALL make placement decisions using host capacity, dependency visibility, tenant or namespace boundaries, and isolation constraints.

#### Scenario: Resource-based scheduling
- **WHEN** a workload requires specific CPU and memory capacity
- **THEN** the scheduler SHALL choose a host that satisfies those constraints or return a placement failure when none exists

### Requirement: Synchronous Placement Feedback
The scheduler guest SHALL expose a request-exchange or equivalent typed interface for placement or scaling intents that require synchronous feedback.

#### Scenario: Placement reply returned
- **WHEN** an external caller submits a placement intent through a synchronous interface
- **THEN** the scheduler SHALL return a success or failure result describing the accepted scheduling outcome

#### Scenario: Placement interface is concrete
- **WHEN** scheduler exposes placement or scaling intent handling
- **THEN** it SHALL use a named request-exchange or typed channel resource rather than only generated metadata

### Requirement: Status Publication
The scheduler guest SHALL publish workload status transitions for subscribers that need to observe scheduling progress.

#### Scenario: Scheduled workload becomes running
- **WHEN** a workload transitions from scheduled to running
- **THEN** the scheduler SHALL publish that status transition through its status topic, live table, or subscription interface

### Requirement: Cluster and Discovery Integration
The scheduler guest SHALL consume host visibility from cluster and resolution data from discovery when those inputs are needed for placement.

#### Scenario: Dependency-aware placement
- **WHEN** a workload depends on another discovered resource
- **THEN** the scheduler SHALL use discovery and cluster inputs when computing the placement decision
