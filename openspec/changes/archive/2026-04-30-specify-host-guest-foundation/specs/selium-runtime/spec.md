## ADDED Requirements

### Requirement: Wasmtiny-Backed Guest Execution
`selium-runtime` SHALL execute Selium guests using Wasmtiny as the WebAssembly runtime substrate.

#### Scenario: Runtime starts a guest module
- **WHEN** the runtime starts a valid guest module
- **THEN** it SHALL instantiate and execute that guest through Wasmtiny rather than a separate WebAssembly runtime

### Requirement: Generic Config-Driven Bootstrap
`selium-runtime` SHALL bootstrap system guests from declarative configuration rather than hard-coded guest-specific startup logic.

#### Scenario: Bootstrap from system guest descriptor
- **WHEN** the host configuration defines a system guest descriptor with module, entrypoint, grants, and bootstrap arguments
- **THEN** the runtime SHALL start that guest generically from the descriptor

### Requirement: Scoped Capability Grants at Spawn
`selium-runtime` SHALL grant capabilities and their selectors to guests at spawn time and SHALL validate those grants before execution begins.

#### Scenario: Spawn with scoped authority
- **WHEN** the runtime starts a guest with a scoped capability grant
- **THEN** the guest SHALL begin execution with that validated authority and no broader ambient authority

### Requirement: Session-Based Grant Persistence
`selium-runtime` SHALL persist capability and scope state as session data so that runtime authority can be resumed consistently across restart and reconnect boundaries where supported.

#### Scenario: Runtime restores session-backed grant state
- **WHEN** the runtime restores a persisted session for a guest or user
- **THEN** it SHALL recover the associated capabilities and selectors from session data

### Requirement: Declarative Dependency and Readiness Handling
`selium-runtime` SHALL support declarative dependencies and readiness conditions for system guest startup.

#### Scenario: Dependent guest waits for readiness
- **WHEN** one configured system guest depends on another guest's readiness condition
- **THEN** the runtime SHALL delay starting or unblocking the dependent guest until the readiness condition is satisfied

### Requirement: Activity Log Projection
`selium-runtime` SHALL project guest lifecycle events into the host activity log using the kernel's activity hooks.

#### Scenario: Guest exits unexpectedly
- **WHEN** a guest process terminates unexpectedly
- **THEN** the runtime SHALL publish a corresponding lifecycle event to the host activity log
