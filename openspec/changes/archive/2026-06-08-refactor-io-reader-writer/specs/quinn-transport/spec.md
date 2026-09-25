## MODIFIED Requirements

### Requirement: UdpSender Implementation
`selium-guest` SHALL implement `quinn::UdpSender` that writes framed datagrams to the shared-memory send channel. The `poll_send` method SHALL be fully implemented using `RingBuf::reserve` and `RingBuf::write_frame`.

#### Scenario: Quinn sends a datagram
- **WHEN** Quinn calls `poll_send` with a `Transmit` containing a destination address and payload
- **THEN** the implementation SHALL encode the destination and payload into a frame, reserve space atomically on the send ring, write the frame with release fencing, and return `Poll::Ready(Ok(()))`

#### Scenario: Quinn sends with full send channel
- **WHEN** Quinn calls `poll_send` and the send channel is full
- **THEN** the implementation SHALL return `Poll::Pending`

### Requirement: AsyncUdpSocket Recv Implementation
`selium-guest` SHALL implement `QuinnUdpSocket::poll_recv` to read framed datagrams from the shared-memory recv channel. The `poll_recv` method SHALL be fully implemented using `RingBuf` read operations with acquire fencing and `FrameHeader` validation.

#### Scenario: Quinn polls for received datagrams
- **WHEN** Quinn's `EndpointDriver` calls `poll_recv` on the socket and a frame with the READY flag set is available in the recv channel
- **THEN** the implementation SHALL read the frame, decode the source address and payload, copy the payload into the provided buffer, populate `RecvMeta` with the source address and length, and return `Poll::Ready(Ok(1))`

#### Scenario: Quinn polls with empty recv channel
- **WHEN** Quinn's `EndpointDriver` calls `poll_recv` on the socket and the recv channel is empty
- **THEN** the implementation SHALL return `Poll::Pending`
