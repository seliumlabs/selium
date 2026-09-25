## MODIFIED Requirements

### Requirement: TCP Stream via Shared-Memory Ring Buffer
`selium-guest` SHALL provide a `TcpStream` handle that uses `Reader` and `Writer` (strong byte-stream handles) over the shared-memory ring buffer for bidirectional socket I/O. `TcpStream` SHALL implement `AsyncRead` and `AsyncWrite` by delegating to its inner `Reader` and `Writer`.

#### Scenario: Guest connects to remote TCP endpoint
- **WHEN** a guest calls `TcpStream::connect(address)` with a valid address
- **THEN** the guest SHALL invoke the `TcpConnect` hostcall, receive a `SharedRegionDescriptor`, attach the region, create `Reader` (inbound) and `Writer` (outbound) handles, and return a `TcpStream` ready for I/O

#### Scenario: Guest reads data from TCP stream
- **WHEN** a guest calls `AsyncRead::poll_read` on a connected `TcpStream`
- **THEN** the implementation SHALL delegate to the inner `Reader::poll_read`, which reads framed data from the inbound ring buffer and copies payload bytes into the provided `ReadBuf`

#### Scenario: Guest writes data to TCP stream
- **WHEN** a guest calls `AsyncWrite::poll_write` on a connected `TcpStream`
- **THEN** the implementation SHALL delegate to the inner `Writer::poll_write`, which writes the data as a framed message to the outbound ring buffer

#### Scenario: Stream EOF from remote end
- **WHEN** the kernel proxy detects the remote TCP peer has closed (read returns 0)
- **THEN** the proxy SHALL decrement the inbound ring's `writer_count` to 0, the inner `Reader::poll_read` SHALL detect `writer_count == 0`, and `TcpStream::poll_read` SHALL return EOF

### Requirement: Quinn UdpSender Implementation
`selium-guest` SHALL implement `QuinnUdpSender::poll_send` to write framed datagrams to the shared-memory send channel using the ring buffer's atomic operations.

#### Scenario: Quinn sends a datagram
- **WHEN** Quinn calls `poll_send` with a `Transmit` containing a destination address and payload
- **THEN** the implementation SHALL encode the destination and payload into a frame and write it to the send ring buffer via `RingBuf::reserve` and `RingBuf::write_frame`

#### Scenario: Quinn sends with full send channel
- **WHEN** Quinn calls `poll_send` and the send channel is full
- **THEN** the implementation SHALL return `Poll::Pending`

### Requirement: Quinn AsyncUdpSocket Recv Implementation
`selium-guest` SHALL implement `QuinnUdpSocket::poll_recv` to read framed datagrams from the shared-memory recv channel using the ring buffer's atomic operations.

#### Scenario: Quinn polls for received datagrams
- **WHEN** Quinn's `EndpointDriver` calls `poll_recv` on the socket and a frame is available in the recv channel
- **THEN** the implementation SHALL read the frame from the recv ring buffer, decode the source address and payload, copy the payload into the provided buffers, and populate the `RecvMeta` with the source address and length

#### Scenario: Quinn polls with empty recv channel
- **WHEN** Quinn's `EndpointDriver` calls `poll_recv` on the socket and the recv channel is empty but writers are still connected
- **THEN** the implementation SHALL return `Poll::Pending`

#### Scenario: Quinn polls with disconnected recv channel
- **WHEN** Quinn's `EndpointDriver` calls `poll_recv` on the socket and the recv channel's `writer_count` is 0
- **THEN** the implementation SHALL return an `io::Error` indicating the channel is closed
