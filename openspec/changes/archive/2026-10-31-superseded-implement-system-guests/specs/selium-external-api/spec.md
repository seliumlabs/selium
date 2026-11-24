## ADDED Requirements

### Requirement: External API Guest Crate
The system SHALL provide a `selium-external-api` guest crate that can be built as a `wasm32-unknown-unknown` system guest and bootstrapped through `selium-runtime` configuration.

#### Scenario: External API guest bootstraps from descriptor
- **WHEN** `selium-runtime` receives a `SystemGuestDescriptor` for `selium-external-api`
- **THEN** it SHALL start the external-api guest using the descriptor's module, zero-argument entrypoint, grants, dependencies, and readiness condition

### Requirement: External Listener Boundary
The external-api guest SHALL accept user-facing sessions through the current network primitive or runtime bridge and SHALL treat QUIC and mTLS identity as configured bridge concerns unless concrete guest-facing support is implemented.

#### Scenario: Authenticated client context received
- **WHEN** the runtime or network bridge provides an authenticated external client context
- **THEN** the external-api guest SHALL accept the session with that context and SHALL NOT duplicate transport authentication policy locally

#### Scenario: Listener bridge configured
- **WHEN** the external-api guest is configured with an external listener
- **THEN** the runtime/network bridge SHALL map that logical listener to a concrete IP/port and route accepted requests to the guest
- **AND** this requirement SHALL remain blocked until the bridge and guest accept/response APIs exist

### Requirement: Intent Interpretation
The external-api guest SHALL interpret external user intent and decompose it into guest-facing interactions.

#### Scenario: Start intent decomposed
- **WHEN** a user requests that replicas of a workload be started
- **THEN** the external-api guest SHALL decompose that request into the discovery and scheduling interactions needed to fulfil it

#### Scenario: Intent is processed from a host-visible request
- **WHEN** a request is delivered through the external-api listener or request-exchange resource
- **THEN** the running guest entrypoint SHALL parse the request and route the resulting intent without requiring the host to call a Rust helper function directly
- **AND** native parser tests SHALL NOT satisfy this scenario by themselves

### Requirement: Narrow Delegation Boundary
The external-api guest SHALL delegate placement, recovery, and discovery policy to the relevant system guests rather than implementing those policies itself.

#### Scenario: Placement delegated
- **WHEN** a user request requires workload placement
- **THEN** the external-api guest SHALL delegate that decision to scheduler rather than making the placement decision locally

#### Scenario: Delegation uses concrete resources
- **WHEN** external-api delegates to scheduler or discovery
- **THEN** it SHALL write to or request through the concrete scheduler/discovery resource names defined for this change
- **AND** metadata-only `#[pattern_interface]` output SHALL NOT be treated as a transport implementation

### Requirement: Explicit Guest Interaction
The external-api guest SHALL use the explicit state, topic, live-table, or request-exchange interface defined for each delegated interaction.

#### Scenario: Synchronous feedback and asynchronous progress
- **WHEN** a user request needs an immediate acceptance result and later progress updates
- **THEN** the external-api guest SHALL combine a request exchange or equivalent typed interface with status-topic subscription as needed

#### Scenario: Client feedback is host-visible
- **WHEN** the running guest accepts or rejects a client request
- **THEN** it SHALL emit the response through a host-visible request exchange, stream, durable log, topic, or live table

### Requirement: Error Propagation
The external-api guest SHALL return meaningful failure context to external callers when delegated interactions fail.

#### Scenario: Delegated request fails
- **WHEN** discovery, scheduler, or another delegated interaction fails
- **THEN** the external-api guest SHALL return an error that identifies the failed step and the relevant context

### Requirement: Metadata Does Not Imply Reachability
The external-api guest SHALL NOT treat generated interface metadata as a callable interface unless a concrete transport or storage surface is also wired.

#### Scenario: Metadata is generated
- **WHEN** `#[pattern_interface]` generates `InterfaceMetadata`
- **THEN** that metadata SHALL be registered or published through a concrete discovery/storage surface before other guests can use it
- **AND** the metadata SHALL NOT be described as RPC by itself

#### Scenario: Metadata remains separate from behaviour
- **WHEN** external-api has no concrete discovery registration path
- **THEN** generated pattern metadata SHALL remain outside the running guest behaviour
