## Purpose

Define the QUIC transport stack for Selium, comprising the shared-memory-backed UDP datagram I/O (`QuinnUdpSocket` in `selium-guest`) that feeds quinn's protocol engine, and the `QuicTransport` in `selium-quic` that wraps quinn streams as `MessageTransport` implementors for use by bridge guests and external clients.

## Requirements

### Requirement: UdpSender Implementation
The QUIC connector guest (`selium-connector-quic`) SHALL implement `quinn::UdpSender` and `quinn::AsyncUdpSocket` over `selium-guest`'s shared-memory `UdpSocket` so the connector can run a quinn endpoint on WASM. These adapter types SHALL live in the connector guest crate rather than in `selium-guest`. They SHALL use `selium-shm` ring buffers for datagram I/O between the guest's quinn stack and the host's UDP socket.

#### Scenario: Quinn sends a datagram via shared-memory
- **WHEN** quinn calls `poll_send` with a `Transmit`
- **THEN** the implementation SHALL encode the destination and payload into a frame on the send ring

#### Scenario: Quinn polls for received datagrams
- **WHEN** quinn's endpoint driver calls `poll_recv` and a frame is available in the recv channel
- **THEN** the implementation SHALL decode the source address and payload, populating `RecvMeta`

### Requirement: Stream Framing Separated from Datagram Transport
The connector guest SHALL own both halves of QUIC data movement: the datagram adapter (`AsyncUdpSocket` over shared-memory rings) and the per-stream byte relay SHALL reside in `selium-connector-quic`. App guests SHALL NOT depend on quinn; the connector SHALL deliver relayed byte channels via `selium-guest::net::quic` over shared memory.

#### Scenario: Bridge guest composes datagram transport + stream transport
- **WHEN** a connector guest (successor to the bridge) initializes quinn with its `AsyncUdpSocket` adapter (implemented in `selium-connector-quic`)
- **AND** relays accepted streams to app guests as per-stream shared-memory byte channels
- **THEN** the external QUIC client SHALL have full connectivity, and the app guest SHALL read and write bytes with no quinn dependency
