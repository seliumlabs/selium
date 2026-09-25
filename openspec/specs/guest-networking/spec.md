## Purpose

Define the guest-facing network SDK surface (`selium-guest` net module): TCP listener/stream and UDP datagram handles backed by shared-memory rings, literals-only addressing, raw-stream escape hatch, and the Quinn adapter implementations.

## Requirements

### Requirement: Guest TCP Listener
`selium-guest` SHALL provide a `TcpListener` whose `bind(address)` issues
the `TcpBind` hostcall and wraps the returned host-queue descriptor in a
`ResourceListener`. `accept()` SHALL yield a `TcpStream` per incoming
connection by attaching the per-connection shared region carried by
`IncomingConnection`.

#### Scenario: Guest accepts a connection
- **WHEN** a remote peer connects to the bound address
- **THEN** the kernel accept loop SHALL push the new stream region's
  `shared_id` onto the listener's host queue, and the guest's `accept()`
  SHALL return a `TcpStream` backed by that region's two rings

#### Scenario: Each connection is a hot-attached region
- **WHEN** a guest accepts N concurrent connections
- **THEN** each connection SHALL be a distinct shared region attached at
  runtime (no upfront declaration), per the hot-swap non-negotiable

### Requirement: Literals-Only Network Addresses
`TcpConnect`, `TcpBind`, and `UdpBind` hostcalls SHALL accept IP literals
only. The runtime SHALL reject any address that fails to parse as a
`SocketAddr` (or `ip:port` equivalent) with
`AbiErrorCode::MalformedPayload`. The host SHALL NOT perform name
resolution on behalf of a guest.

#### Scenario: Hostname connect rejected
- **WHEN** a guest issues `TcpConnect` with `"example.com:443"`
- **THEN** the runtime SHALL return `MalformedPayload` and SHALL NOT
  create a socket, region, or DNS query

#### Scenario: Literal connect accepted
- **WHEN** a guest issues `TcpConnect` with `"93.184.216.34:443"`
- **THEN** the runtime SHALL proceed with the OS connect and return the
  stream region descriptor

### Requirement: Guest UDP Socket Datagram API
`selium-guest` SHALL provide a `UdpSocket` with
`poll_send(Datagram { addr, payload })` and
`poll_recv() -> Poll<Datagram>` over the two-ring datagram region.
Datagrams SHALL be encoded on the ring in the binary format defined by
the `udp-transport` spec. The API shape SHALL be consumable by a future
`quinn` `AsyncUdpSocket` adapter without guest-visible changes.

#### Scenario: Guest sends a datagram
- **WHEN** a guest calls `poll_send` with a `Datagram`
- **THEN** the SDK SHALL encode the binary datagram frame on the send
  ring, and the kernel proxy SHALL emit a UDP datagram to `addr`

#### Scenario: Guest receives a datagram
- **WHEN** a datagram arrives on the bound OS socket
- **THEN** the kernel proxy SHALL encode source address and payload as a
  binary frame, and `poll_recv` SHALL return the decoded `Datagram`

### Requirement: Raw Stream Escape Hatch
The `AsyncRead + AsyncWrite` implementation on `TcpStream` SHALL register
wakers through `register_generation_wait` and SHALL NOT busy-poll or
self-wake on every poll. Raw stream access SHALL remain public API
regardless of any higher-level protocol overlays built on it.

#### Scenario: BYO framework drives the stream
- **WHEN** a user wraps `TcpStream` in `hyper_util::rt::TokioIo` and
  polls it from a guest task
- **THEN** pending reads SHALL park the task until ring generation
  advances, and pending writes SHALL obey ring backpressure

### Requirement: TCP Stream via Shared-Memory Ring Buffer
`selium-guest` SHALL provide a `TcpStream` handle that uses `Reader` and
`Writer` (strong byte-stream handles) over the shared-memory ring buffer
for bidirectional socket I/O. `TcpStream` SHALL implement `AsyncRead` and
`AsyncWrite` by delegating to its inner `Reader` and `Writer`. Connect
addresses SHALL be IP literals (see Literals-Only Network Addresses).

#### Scenario: Guest connects to remote TCP endpoint
- **WHEN** a guest calls `TcpStream::connect(address)` with a valid
  IP-literal address
- **THEN** the guest SHALL invoke the `TcpConnect` hostcall, receive a
  `SharedRegionDescriptor`, attach the region, create `Reader` (inbound)
  and `Writer` (outbound) handles, and return a `TcpStream` ready for I/O

#### Scenario: Guest reads data from TCP stream
- **WHEN** a guest calls `AsyncRead::poll_read` on a connected `TcpStream`
- **THEN** the implementation SHALL delegate to the inner
  `Reader::poll_read`, which reads framed data from the inbound ring
  buffer and copies payload bytes into the provided `ReadBuf`

#### Scenario: Guest writes data to TCP stream
- **WHEN** a guest calls `AsyncWrite::poll_write` on a connected `TcpStream`
- **THEN** the implementation SHALL delegate to the inner
  `Writer::poll_write`, which writes the data as a framed message to the
  outbound ring buffer

#### Scenario: Stream EOF from remote end
- **WHEN** the kernel proxy detects the remote TCP peer has closed (read
  returns 0)
- **THEN** the proxy SHALL decrement the inbound ring's `writer_count` to
  0, the inner `Reader::poll_read` SHALL detect `writer_count == 0`, and
  `TcpStream::poll_read` SHALL return EOF

#### Scenario: Frame boundaries are not messages
- **WHEN** the kernel proxy writes socket data to the inbound ring in
  arbitrary read-chunk frames
- **THEN** `TcpStream` reads SHALL present a continuous byte stream and
  SHALL NOT expose frame boundaries as message boundaries

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
