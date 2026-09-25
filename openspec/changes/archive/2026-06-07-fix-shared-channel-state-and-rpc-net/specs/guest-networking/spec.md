## ADDED Requirements

### Requirement: TCP Stream via Shared-Memory Ring Buffer
`selium-guest` SHALL provide a `TcpStream` handle that uses the shared-memory ring buffer for bidirectional socket I/O.

#### Scenario: Guest connects to remote TCP endpoint
- **WHEN** a guest calls `TcpStream::connect(address)` with a valid address
- **THEN** the guest SHALL invoke the `TcpConnect` hostcall, receive a `SharedRegionDescriptor`, attach the region, create inbound and outbound `RingBuf` handles, and return a `TcpStream` ready for I/O

#### Scenario: Guest reads data from TCP stream
- **WHEN** a guest calls `AsyncRead::poll_read` on a connected `TcpStream`
- **THEN** the implementation SHALL read framed data from the inbound ring buffer and copy it into the provided `ReadBuf`

#### Scenario: Guest writes data to TCP stream
- **WHEN** a guest calls `AsyncWrite::poll_write` on a connected `TcpStream`
- **THEN** the implementation SHALL write the data as a framed message to the outbound ring buffer and bump the generation counter

#### Scenario: Stream EOF from remote end
- **WHEN** the kernel proxy detects the remote TCP peer has closed (read returns 0)
- **THEN** the proxy SHALL decrement the inbound ring's `writer_count` to 0, and the guest SHALL detect this and return EOF from `poll_read`

### Requirement: TCP Listener via HostQueue
`selium-guest` SHALL provide a `TcpListener` that accepts incoming connections via the `HostQueue` mechanism and produces `TcpStream` handles.

#### Scenario: Guest accepts incoming TCP connection
- **WHEN** a guest calls `TcpListener::accept()` and a connection is pending in the host queue
- **THEN** the listener SHALL receive the `IncomingConnection`, attach to the shared region identified by `shared_id`, and return a working `TcpStream`

#### Scenario: Guest binds TCP listener
- **WHEN** a guest calls `TcpListener::bind(address)` with a valid address
- **THEN** the guest SHALL invoke the `TcpBind` hostcall, receive a `HostQueueDescriptor`, wrap it in a `ResourceListener`, and return a `TcpListener`

### Requirement: UDP Socket via Shared-Memory Ring Buffer
`selium-guest` SHALL provide a `UdpSocket` handle that uses shared-memory ring buffers for datagram I/O.

#### Scenario: Guest binds UDP socket
- **WHEN** a guest calls `UdpSocket::bind(address)` with a valid address
- **THEN** the guest SHALL invoke the `UdpBind` hostcall, receive a `SharedRegionDescriptor`, attach the region, create recv and send `RingBuf` handles, and return a `UdpSocket`

#### Scenario: Guest receives datagram
- **WHEN** a guest reads from a bound `UdpSocket`
- **THEN** the implementation SHALL read a framed datagram from the recv ring buffer, decode the source address and payload, and return them

#### Scenario: Guest sends datagram
- **WHEN** a guest sends a datagram to a destination address
- **THEN** the implementation SHALL encode the destination address and payload as a frame, write it to the send ring buffer, and bump the generation counter

### Requirement: Network Ring Buffer Layout
TCP streams and UDP sockets SHALL use a multi-memory region containing two ring buffers with the standard coordination layout (generation counter, next_tail, writer_count, reader_slots in page 0; data in page 1+).

#### Scenario: Guest and kernel proxy share consistent layout
- **WHEN** a guest attaches a network region returned by the kernel
- **THEN** the guest SHALL discover two ring buffers via the multi-memory header and coordinate through shared-memory atomics at the standard offsets
