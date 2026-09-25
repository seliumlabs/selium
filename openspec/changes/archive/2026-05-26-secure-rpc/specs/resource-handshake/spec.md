## ADDED Requirements

### Requirement: Host-mediated connection queue
The system SHALL provide a host-mediated queue for establishing shared memory connections between guests. The queue SHALL be accessed through `ResourceSender` (client side) and `ResourceListener` (server side). The host SHALL validate capability before enqueuing connections.

#### Scenario: Client sends connection request
- **WHEN** a client calls `ResourceSender::send(handle, shared_id)`
- **THEN** the system SHALL invoke the `HostQueueSend` hostcall, which validates the client's capability to reach the target server and enqueues the connection information

#### Scenario: Server accepts connection
- **WHEN** a server calls `listener.accept::<T>().await`
- **THEN** the system SHALL return an `IncomingConnection` containing the `client_process_id` and `shared_id`, which the `Accept` implementation uses to construct the typed connection

### Requirement: Typed Accept trait
The system SHALL provide an `Accept` trait that maps raw incoming connections to typed resources. The trait SHALL have a single associated type `Item` and a fallible `accept` function.

#### Scenario: Accepting an RPC connection
- **WHEN** a server calls `listener.accept::<RpcAccept<Req, Rep>>().await`
- **THEN** the system SHALL invoke `RpcAccept::accept` which validates the region layout and constructs an `RpcConnection<Req, Rep>`

#### Scenario: Accepting a different resource type
- **WHEN** a future resource type (e.g. process) defines its own `Accept` implementation
- **THEN** the same `ResourceListener` SHALL be usable with `listener.accept::<ProcessAccept>()` to accept process connections

#### Scenario: Malformed connection rejected
- **WHEN** an incoming connection has an invalid region (bad magic, wrong memory count, unreadable layout)
- **THEN** `Accept::accept` SHALL return `AcceptError::InvalidRegion` or `AcceptError::LayoutMismatch`

### Requirement: Backpressure on send
`ResourceSender::send` SHALL be async and MAY block if the server has not consumed previous connection entries. The future SHALL resolve when the host has successfully enqueued the connection.

#### Scenario: Server is slow to accept
- **WHEN** a client calls `send` and the server's connection queue is full
- **THEN** the future SHALL not resolve until the server accepts a connection and frees space in the queue

### Requirement: ResourceListener receives client identity
When a connection is dequeued via `ResourceListener::accept`, the system SHALL provide the `ProcessId` of the connecting client alongside the `shared_id` of the session region.

#### Scenario: Server receives client identity
- **WHEN** a server accepts a connection
- **THEN** the `IncomingConnection` SHALL contain `client_process_id` and `session_shared_id`