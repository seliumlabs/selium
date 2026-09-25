## MODIFIED Requirements

### Requirement: Quinn Transport as MessageTransport
`selium-quic` SHALL provide a `QuicTransport` type that implements `MessageTransport` over quinn `SendStream`/`RecvStream`. The type SHALL be usable by both the external client library and bridge guests. The existing UDP datagram send/recv infrastructure (`QuinnUdpSocket`, `SeliumQuinnRuntime`) SHALL remain in `selium-guest` as the datagram-level transport feeding quinn.

#### Scenario: Quinn stream read via QuicTransport
- **WHEN** `QuicTransport::poll_read` is called and data is available on the quinn `RecvStream`
- **THEN** bytes SHALL be copied into the provided buffer, matching `AsyncRead` semantics

#### Scenario: Quinn stream write via QuicTransport
- **WHEN** `QuicTransport::poll_write` is called with frame bytes
- **THEN** bytes SHALL be written to the quinn `SendStream`

#### Scenario: QuicTransport signals peer closed
- **WHEN** the remote peer closes the QUIC stream (FIN or RESET)
- **THEN** `QuicTransport::poll_peer_closed` SHALL return `Ok(true)`

### Requirement: UdpSender Implementation
`selium-guest` SHALL continue to implement `quinn::UdpSender` and `quinn::AsyncUdpSocket` over shared-memory send/recv channels for WASM guests that need QUIC. These types SHALL use `selium-shm` ring buffers for datagram I/O between the guest's quinn stack and the host's UDP socket.

#### Scenario: Quinn sends a datagram via shared-memory
- **WHEN** Quinn calls `poll_send` with a `Transmit`
- **THEN** the implementation SHALL encode the destination and payload into a frame on the send ring

#### Scenario: Quinn polls for received datagrams
- **WHEN** Quinn's endpoint driver calls `poll_recv` and a frame is available in the recv channel
- **THEN** the implementation SHALL decode the source address and payload, populating `RecvMeta`

### Requirement: Stream Framing Separated from Datagram Transport
The stream-level frame handling (`QuicTransport` implementing `MessageTransport`) SHALL reside in `selium-quic`. The datagram transport (`QuinnUdpSocket` with shared-memory rings) SHALL remain in `selium-guest`. Bridge guests SHALL use both: the datagram transport to run quinn, and `selium-quic` to wrap streams.

#### Scenario: Bridge guest composes datagram transport + stream transport
- **WHEN** a bridge guest initializes quinn with `QuinnUdpSocket` (from `selium-guest`)
- **AND** wraps accepted streams in `QuicTransport` (from `selium-quic`)
- **THEN** the bridge SHALL have full QUIC connectivity and stream-level `MessageTransport` semantics
