## ADDED Requirements

### Requirement: Supervisor Guest Crate
The system SHALL provide a `selium-supervisor` guest crate that can be built as a `wasm32-unknown-unknown` system guest and bootstrapped through `selium-runtime` configuration.

#### Scenario: Supervisor guest bootstraps from descriptor
- **WHEN** `selium-runtime` receives a `SystemGuestDescriptor` for `selium-supervisor`
- **THEN** it SHALL start the supervisor guest using the descriptor's module, zero-argument entrypoint, grants, dependencies, and readiness condition

### Requirement: Runtime Activity Subscription
The supervisor guest SHALL subscribe to or poll runtime activity and lifecycle signals for the processes it manages using the current activity-log interface.

#### Scenario: Process lifecycle event observed
- **WHEN** the runtime publishes a lifecycle event for a managed process
- **THEN** the supervisor SHALL receive and process that event

#### Scenario: Running supervisor reads activity
- **WHEN** the supervisor guest entrypoint starts with activity authority
- **THEN** it SHALL read or subscribe to the runtime activity-log resource before reporting readiness or before entering its observation loop
- **AND** native-only health helpers SHALL NOT satisfy runtime activity subscription

### Requirement: Managed Health State
The supervisor guest SHALL maintain health and failure state for managed processes.

#### Scenario: Process health updated
- **WHEN** the supervisor observes a health signal or failure event for a managed process
- **THEN** it SHALL update the managed health state for that process

### Requirement: Restart Policy Evaluation
The supervisor guest SHALL evaluate configured restart policies, including immediate restart, on-failure restart, no restart, and backoff-based restart.

#### Scenario: Backoff policy applied
- **WHEN** a managed process fails under a backoff restart policy
- **THEN** the supervisor SHALL delay recovery according to the configured backoff rules before emitting restart intent

### Requirement: Recovery Intent Emission
The supervisor guest SHALL emit recovery or rescheduling intent through an explicit scheduler-facing state, topic, or request-exchange interface rather than through host-specific recovery logic.

#### Scenario: Restart requested
- **WHEN** the supervisor determines that a process should be restarted or rescheduled
- **THEN** it SHALL emit that intent through the appropriate guest-facing interface

#### Scenario: Recovery intent is concrete
- **WHEN** supervisor emits restart or rescheduling intent
- **THEN** it SHALL write to a named scheduler-facing topic, live table, durable log, or request exchange

### Requirement: Rules-Based Failure Handling
The supervisor guest SHALL apply configured rules when handling unplanned process termination.

#### Scenario: Unexpected termination handled
- **WHEN** a managed process terminates unexpectedly
- **THEN** the supervisor SHALL evaluate the matching rules and choose the corresponding recovery action
