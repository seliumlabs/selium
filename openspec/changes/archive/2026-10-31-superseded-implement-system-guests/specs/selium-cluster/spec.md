## ADDED Requirements

### Requirement: Cluster Guest Crate
The system SHALL provide a `selium-cluster` guest crate that can be built as a `wasm32-unknown-unknown` system guest and bootstrapped through `selium-runtime` configuration.

#### Scenario: Cluster guest bootstraps from descriptor
- **WHEN** `selium-runtime` receives a `SystemGuestDescriptor` for `selium-cluster`
- **THEN** it SHALL start the cluster guest using the descriptor's module, zero-argument entrypoint, grants, dependencies, and readiness condition

### Requirement: Host Membership Tracking
The cluster guest SHALL track host membership and availability for the cluster it belongs to.

#### Scenario: Host joins cluster
- **WHEN** a host becomes visible to the configured cluster coordination mechanism
- **THEN** the cluster guest SHALL record that host in cluster membership state

#### Scenario: Running guest owns membership state
- **WHEN** the cluster guest entrypoint starts
- **THEN** it SHALL create, attach, or recover the configured membership table/log resource before reporting readiness
- **AND** native-only state helpers SHALL NOT satisfy membership tracking without entrypoint wiring

### Requirement: Host Load Projection
The cluster guest SHALL expose host load and availability data for consumers such as scheduler.

#### Scenario: Scheduler reads host load
- **WHEN** scheduler needs host capacity and availability inputs
- **THEN** the cluster guest SHALL provide the current host load view through the defined state, topic, or live-table interface

#### Scenario: Host load surface is concrete
- **WHEN** host load changes are published
- **THEN** the cluster guest SHALL write them to a named live table, topic, durable log, or request-exchange response that scheduler can consume

### Requirement: Shared-State Bootstrap
The cluster guest SHALL initialise the day 1 shared state surfaces needed by other system guests.

#### Scenario: First host bootstraps shared state
- **WHEN** the first host in a cluster starts without existing shared state
- **THEN** the cluster guest SHALL initialise the cluster-visible state surfaces required for discovery, scheduler, supervisor, and external-api

### Requirement: Protocol-Neutral Host Coordination
The cluster guest SHALL use the current foundation network and I/O primitives for host coordination and SHALL NOT assume guest-owned QUIC or mTLS support unless that support is implemented.

#### Scenario: Host exchanges cluster state
- **WHEN** two hosts exchange cluster coordination data
- **THEN** the cluster guest SHALL use configured listener, session, stream, request-exchange, topic, or live-table primitives rather than guest-specific ad hoc transport

### Requirement: External Bootstrap Visibility
The cluster guest SHALL expose sufficient cluster-address visibility for external bootstrap and discovery flows.

#### Scenario: External bootstrap addresses projected
- **WHEN** the platform needs to expose bootstrap addresses for external discovery
- **THEN** the cluster guest SHALL publish or project the configured address set through the defined external discovery mechanism
