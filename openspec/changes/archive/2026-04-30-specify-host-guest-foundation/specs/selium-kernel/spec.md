## ADDED Requirements

### Requirement: Shared Memory Regions
`selium-kernel` SHALL expose shared memory regions as first-class primitive resources that can be allocated, attached, detached, and accessed independently of a guest's private linear memory.

#### Scenario: Shared region attached to two guests
- **WHEN** two guests attach the same valid shared memory region
- **THEN** both guests SHALL be able to access the region according to the runtime memory model

### Requirement: Explicit Signalling Primitive
`selium-kernel` SHALL expose an explicit wait/notify coordination primitive that does not require request/reply or queue semantics.

#### Scenario: Guest waits for shared-memory update
- **WHEN** one guest publishes a readiness signal after updating shared state
- **THEN** another guest waiting on that signal SHALL be able to resume without polling blindly

### Requirement: Protocol-Neutral Network Primitives
`selium-kernel` SHALL expose protocol-neutral listener, session, stream, and request/response network primitives.

#### Scenario: Guest opens outbound stream
- **WHEN** a guest with the required network capability opens an outbound stream
- **THEN** the kernel SHALL provide a stream resource without embedding higher-level messaging semantics into the primitive

### Requirement: Durable Storage Primitives
`selium-kernel` SHALL expose durable log and blob primitives with append, replay, checkpoint, put, and get operations.

#### Scenario: Guest replays a durable log
- **WHEN** a guest replays a durable log from a valid checkpoint or sequence
- **THEN** the kernel SHALL return the retained records and bounds according to the storage contract

### Requirement: Primitive Process Lifecycle
`selium-kernel` SHALL expose primitive operations for starting, stopping, and inspecting guest processes without embedding placement or orchestration policy.

#### Scenario: Runtime starts configured guest process
- **WHEN** the runtime requests a new guest process using a valid module and entrypoint
- **THEN** the kernel SHALL create the process resource and return an inspectable process identity

### Requirement: Activity and Metering Hooks
`selium-kernel` SHALL expose hooks that allow the runtime to project lifecycle events and resource-usage observations into host-visible logs and metering streams.

#### Scenario: Guest process consumes resources
- **WHEN** a guest process uses CPU, memory, storage, or bandwidth
- **THEN** the kernel SHALL make those observations available to the runtime through the metering hooks
