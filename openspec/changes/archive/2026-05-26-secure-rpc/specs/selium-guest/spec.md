## MODIFIED Requirements

### Requirement: Safe Guest Handles
`selium-guest` SHALL provide safe, ergonomic handle types over ABI primitives so guest code does not manipulate raw hostcall payloads directly for common operations.

#### Scenario: ResourceSender handle
- **WHEN** guest code needs to send a shared resource identifier to a service's connection queue
- **THEN** the SDK SHALL provide a `ResourceSender` handle type that wraps the `HostQueueSend` hostcall

#### Scenario: ResourceListener handle
- **WHEN** guest code needs to accept incoming connections for an RPC service
- **THEN** the SDK SHALL provide a `ResourceListener` handle type that wraps the `HostQueueRecv` hostcall and provides typed `accept` via the `Accept` trait

### Requirement: Messaging-Pattern Layer
`selium-guest` SHALL provide a messaging-pattern layer built above the primitive substrate.

#### Scenario: Guest selects request/reply pattern
- **WHEN** guest code needs request/reply semantics for inter-guest communication
- **THEN** the SDK SHALL provide `RpcClient` and `RpcConnection` types through the `selium-io::rpc` module rather than requiring guest-specific boilerplate