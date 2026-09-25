## ADDED Requirements

### Requirement: MessageTransport Trait
The system SHALL provide a `MessageTransport` trait in `selium-wire` that abstracts a duplex framed I/O channel. The trait SHALL compose `tokio::io::AsyncRead + AsyncWrite + Unpin` and SHALL add:

- `poll_ready(&mut self, cx: &mut Context) -> Result<bool>` — returns whether a complete frame is immediately readable
- `poll_peer_closed(&mut self, cx: &mut Context) -> Result<bool>` — returns whether the remote peer has disconnected
- `generation(&self) -> Result<u64>` — returns the current generation counter, or zero if the transport does not support generation tracking

#### Scenario: Transport signals data available
- **WHEN** a frame is ready on the underlying medium
- **THEN** `poll_ready` SHALL return `Ok(true)`

#### Scenario: Transport signals empty
- **WHEN** no frame is available and the peer is still connected
- **THEN** `poll_ready` SHALL return `Ok(false)`

#### Scenario: Transport signals peer closed
- **WHEN** the remote peer disconnects (FIN, RESET_STREAM, or writer_count reaches zero)
- **THEN** `poll_peer_closed` SHALL return `Ok(true)`

### Requirement: Patterns Generic Over MessageTransport
`Publisher<T, M>`, `Subscriber<T, M>`, `RpcClient<Req, Rep, M>`, and `RpcConnection<Req, Rep, M>` SHALL be generic over a type parameter `M: MessageTransport`. Each pattern SHALL use only the trait's methods — never reaching into concrete transport internals.

#### Scenario: Publisher is instantiated with shm transport
- **WHEN** `Publisher<u64, ShmTransport>` is constructed
- **THEN** `publish` SHALL work identically to the current `selium-guest` publisher

#### Scenario: Publisher is instantiated with QUIC transport
- **WHEN** `Publisher<u64, QuicTransport>` is constructed
- **THEN** `publish` SHALL encode the same frame format over a QUIC stream

#### Scenario: Subscriber detects overwrite over shm transport
- **WHEN** a `Subscriber` backed by an shm transport calls `poll_next` and the generation delta exceeds capacity
- **THEN** the subscriber SHALL return `Err(Error::Overwritten)`

#### Scenario: Subscriber does not report overwrite over QUIC transport
- **WHEN** a `Subscriber` backed by a QUIC transport calls `poll_next`
- **THEN** overwrite SHALL NOT be reported (the transport sets `generation()` to zero; overwrite is not a QUIC concept)

### Requirement: Frame Format Transport-Independent
The frame header layout (`FrameHeader` with tag, flags, and length fields) SHALL be identical regardless of whether the transport is shared-memory or QUIC. `FramedRead<M>` and `FramedWrite<M>` SHALL be generic over `M: MessageTransport`.

#### Scenario: Frame written over shm is readable over shm
- **WHEN** a `FramedWrite<ShmTransport>` writes a frame with tag 42
- **THEN** a `FramedRead<ShmTransport>` attached to the same ring SHALL read a frame with tag 42 and the same payload

#### Scenario: Frame written over QUIC is readable over QUIC
- **WHEN** a `FramedWrite<QuicTransport>` writes a frame with tag 42
- **THEN** a `FramedRead<QuicTransport>` on the receiving QUIC stream SHALL read a frame with tag 42 and the same payload

### Requirement: Transport Error Abstraction
The `MessageTransport` trait SHALL define an associated `Error` type, and all pattern-level error types (`RpcError`, pubsub channel errors) SHALL be constructible from transport errors via a `From` conversion.

#### Scenario: RpcError from transport error
- **WHEN** the underlying transport returns an error during an RPC operation
- **THEN** the `RpcError` variant SHALL preserve the transport error's semantics (e.g., `BufferFull` → `RpcError::BufferFull`, peer closed → `ConnectionClosed`)
