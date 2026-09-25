## Purpose

Define bidirectional, isolated RPC sessions with typed request/reply, correlation IDs, and connection close detection.

## Requirements

### Requirement: Bidirectional RPC session isolation
The system SHALL establish per-connection RPC sessions between a client and a server, where each session occupies a dedicated `SharedRegion` that no other guest can access. The host SHALL only grant the session's `shared_id` to the two authorised parties.

#### Scenario: Client connects to server
- **WHEN** a client calls `RpcClient::connect` with a valid server handle
- **THEN** the system SHALL allocate a `SharedRegion` with two sub-memories (request ring and reply ring), send the `shared_id` through the host-mediated queue, and return a connected `RpcClient`

#### Scenario: Unauthorised client attempts connection
- **WHEN** a client attempts to connect to a server for which it lacks capability
- **THEN** the host SHALL reject the connection at the `HostQueueSend` hostcall and return an error

#### Scenario: Malicious guest cannot access another tenant's session
- **WHEN** a guest does not possess the `shared_id` for a session
- **THEN** the guest SHALL NOT be able to attach to or read that session's shared memory

### Requirement: Typed request/reply with correlation
The system SHALL provide typed RPC with compile-time type safety. `RpcClient<Req, Rep>` SHALL serialise requests of type `Req` and deserialise replies of type `Rep`. `RpcConnection<Req, Rep>` SHALL deserialise requests of type `Req` and serialise replies of type `Rep`. Correlation between requests and replies SHALL use the `tag` field in `FrameHeader`.

#### Scenario: Client sends typed request and receives typed reply
- **WHEN** a client calls `client.request(payload)` where `payload` is of type `Req`
- **THEN** the system SHALL serialise the payload, write it to the request ring with a unique correlation ID, await the matching correlation ID on the reply ring, and deserialise and return the reply as type `Rep`

#### Scenario: Correlation ID wraps around
- **WHEN** the correlation ID counter reaches `u32::MAX`
- **THEN** the system SHALL wrap to 0 and continue assigning IDs sequentially

### Requirement: One-reply-per-request enforcement
The system SHALL enforce exactly-one-reply semantics per request. `RpcRequest<Req, Rep>` SHALL consume `self` on reply, preventing duplicate responses.

#### Scenario: Server replies to a request
- **WHEN** the server calls `request.reply(response)` on an `RpcRequest`
- **THEN** the system SHALL serialise the response, write it to the reply ring with the request's correlation ID, and consume the `RpcRequest` handle

#### Scenario: Server attempts to reply twice
- **WHEN** the server attempts to call `reply` on a `RpcRequest` that has already been consumed
- **THEN** the Rust type system SHALL prevent the call at compile time (the method takes `self`, not `&self`)

### Requirement: Connection close detection
The system SHALL detect connection termination when the peer drops their handle. When all writers on a ring have disconnected (writer count reaches zero), the peer SHALL receive `Error::ConnectionClosed` on subsequent read attempts.

#### Scenario: Client drops RpcClient
- **WHEN** the client side drops its `RpcClient`
- **THEN** the request ring's writer count SHALL decrement to zero, and the server's next `recv` call SHALL return `Error::ConnectionClosed`

#### Scenario: Server drops RpcConnection
- **WHEN** the server side drops its `RpcConnection`
- **THEN** the reply ring's writer count SHALL decrement to zero, and the client's next `request` call SHALL return `Error::ConnectionClosed`

### Requirement: FrameHeader tag field for RPC correlation
The `FrameHeader` `tag` field SHALL serve as `correlation_id` in RPC contexts. Each request frame SHALL carry the correlation ID assigned by the client. Each reply frame SHALL carry the same correlation ID so the client can match replies to requests.

#### Scenario: Client matches reply to request
- **WHEN** a client has multiple in-flight requests with correlation IDs 1, 2, and 3
- **THEN** the client SHALL match each reply frame's `tag` field to the corresponding in-flight request and return the deserialised reply to the correct caller
