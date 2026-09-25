## MODIFIED Requirements

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
