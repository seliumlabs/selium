## Purpose

Define the `Rendezvous` trait and host-mediated connection queue for establishing shared-memory sessions between clients and servers, abstracting the mechanism for passing connection identifiers while preserving capability validation and backpressure.

## Requirements

### Requirement: Rendezvous Trait for Connection Establishment
`selium-wire` SHALL provide a `Rendezvous` trait that abstracts the mechanism for passing connection identifiers from client to server. The trait SHALL define:

- `async fn send(&self, shared_id: u64) -> Result<()>`
- `async fn recv(&self) -> Result<IncomingConnection>`
- `fn attach_sender(shared_id: u64) -> Result<Self>` (for clients)
- `fn create_listener() -> Result<Self>` (for servers)

The existing `ResourceSender`/`ResourceListener` hostcall-backed mechanism SHALL be one concrete `Rendezvous` implementation for WASM guests. The runtime MAY provide its own `Rendezvous` implementation for native-to-guest RPC without using hostcalls.

#### Scenario: Client sends connection via rendezvous
- **WHEN** a client calls `rendezvous.send(shared_id)`
- **THEN** the implementation SHALL deliver the `shared_id` to the server's `recv()` endpoint

#### Scenario: Server accepts connection via rendezvous
- **WHEN** a server calls `rendezvous.recv().await`
- **THEN** it SHALL receive an `IncomingConnection` containing `client_process_id` and `shared_id`

### Requirement: Host-mediated Connection Queue (ResourceSender/ResourceListener)
The `ResourceSender` and `ResourceListener` types in `selium-guest` SHALL implement the `Rendezvous` trait. The host SHALL continue to validate capability before enqueuing connections.

#### Scenario: Client sends connection request via ResourceSender
- **WHEN** a client calls `sender.send(shared_id).await`
- **THEN** the system SHALL invoke the `HostQueueSend` hostcall with capability validation

#### Scenario: Server accepts connection via ResourceListener
- **WHEN** a server calls `listener.recv().await`
- **THEN** the system SHALL return an `IncomingConnection` with `client_process_id` and `shared_id`

### Requirement: Typed Accept Trait
The `Accept` trait SHALL remain in `selium-guest`, mapping raw `IncomingConnection` values to typed resources. RPC acceptance MAY also be implemented via direct `Rendezvous` usage without the `Accept` trait for non-guest consumers.

#### Scenario: Accepting an RPC connection via Accept trait
- **WHEN** a server calls `listener.accept::<RpcAccept<Req, Rep>>().await`
- **THEN** `RpcAccept::accept` SHALL construct an `RpcConnection<_, _, ShmTransport>`

### Requirement: Backpressure on Send
`Rendezvous::send` SHALL be async and MAY block if the server has not consumed previous entries. The future SHALL resolve when the connection has been enqueued.

#### Scenario: Server is slow to accept
- **WHEN** a client calls `send` and the server's queue is full
- **THEN** the future SHALL not resolve until the server accepts a connection

### Requirement: Connection Identity
When a connection is received via `Rendezvous::recv`, the `IncomingConnection` SHALL contain the `client_process_id` (or equivalent peer identity) alongside the `shared_id`.

#### Scenario: Server receives client identity
- **WHEN** a server accepts a connection
- **THEN** the `IncomingConnection` SHALL contain the peer identity and `shared_id`

### Requirement: Queue Attach Authorisation

`HostQueueAttach` against a host queue owned by another process SHALL
succeed only when the caller holds an `ExplicitResource` grant naming
that queue, or the caller obtained the queue's shared id through a
successful discovery resolution performed by the caller. Attach attempts
without either basis SHALL be denied with a capability error. A process
SHALL always be permitted to attach to queues it created itself.

#### Scenario: Ungranted attach denied

- **WHEN** a process attempts `HostQueueAttach` on a queue owned by
  another process without a grant naming it and without having resolved
  it via discovery
- **THEN** the hostcall is denied with a capability error

#### Scenario: Discovery-resolved attach permitted

- **WHEN** a connector resolves a URI subtree via discovery and uses the
  returned queue id to attach as a sender
- **THEN** the attach succeeds because the descriptor was obtained
  through discovery resolution

#### Scenario: Owner attach always permitted

- **WHEN** a process attaches to a host queue it created itself
- **THEN** the attach succeeds without additional grants

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
