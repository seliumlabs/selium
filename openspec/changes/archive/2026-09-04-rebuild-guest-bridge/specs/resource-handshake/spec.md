## ADDED Requirements

### Requirement: Handoff Metadata
`HostQueueSend` SHALL optionally carry an opaque metadata payload, and the corresponding `HostQueueRecv` SHALL surface that payload in `IncomingConnection`. Sending without metadata SHALL yield an empty metadata value at the receiver. The payload SHALL be bounded; a `HostQueueSend` whose metadata exceeds the bound SHALL be rejected before the payload reaches the kernel queue.

#### Scenario: Metadata delivered
- **WHEN** a sender enqueues a connection with an opaque metadata payload within the bound
- **THEN** the receiver's `IncomingConnection` SHALL contain exactly that payload

#### Scenario: No metadata
- **WHEN** a sender enqueues a connection without metadata
- **THEN** the receiver's `IncomingConnection` SHALL contain an empty metadata value

#### Scenario: Oversized metadata rejected
- **WHEN** a sender enqueues a connection whose metadata payload exceeds the bound
- **THEN** the hostcall SHALL fail with a malformed-payload error and the queue SHALL be unchanged

### Requirement: Pinned Handoff Sender
Because handoff metadata is sender-controlled and opaque, a serve-side listener SHALL be able to pin the single process allowed to deliver handoffs to it. Handoffs from any other process SHALL be refused by attaching and immediately closing the delivered region (so the sender observes EOF rather than parking), and SHALL NOT be surfaced to the listener's caller. The pinned sender SHALL be resolvable from the runtime's bootstrap-authoritative protocol handler registry, which guests cannot forge.

#### Scenario: Handoff from unpinned sender refused
- **WHEN** a listener pinned to process P receives a handoff from a different process Q
- **THEN** the handoff SHALL be refused (attach-then-close) and the listener SHALL continue waiting for a handoff from P

#### Scenario: Handler resolution is authoritative
- **WHEN** a guest resolves the registered protocol handler for a scheme
- **THEN** the runtime SHALL return the process id of the bootstrap-registered handler for that scheme, and no guest SHALL be able to register or forge a handler

### Requirement: Guest Self Identity
A guest SHALL be able to query its own process id and tenant scope.

#### Scenario: Self info
- **WHEN** a guest queries its own identity
- **THEN** the host SHALL return the guest's process id and provisioned tenant scope
