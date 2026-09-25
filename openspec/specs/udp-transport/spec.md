## Purpose

Define the UDP transport protocol for Selium, covering socket binding, datagram framing, resource lifecycle, and capability-gated access control.

## Requirements

### Requirement: UDP Socket Binding via Hostcall
`selium-abi` SHALL define a `UdpBind` hostcall variant that accepts a socket address and returns a `SharedRegionDescriptor` containing the ring buffers and signal metadata for UDP communication.

#### Scenario: Guest binds a UDP socket
- **WHEN** a guest invokes `HostcallRequest::UdpBind` with a valid local address
- **THEN** the host SHALL bind a real OS UDP socket at that address and return a `SharedRegionDescriptor` referencing a shared memory region with two ring buffers (recv and send) and their associated signal ids embedded in the region headers

#### Scenario: Guest binds with an invalid address
- **WHEN** a guest invokes `UdpBind` with an address that cannot be bound (e.g. port in use, invalid address format)
- **THEN** the hostcall SHALL return an explicit `AbiError` with a descriptive message

### Requirement: Binary Datagram Frame Format
Datagram frames exchanged between a guest `UdpSocket` and the kernel UDP
proxy SHALL use a binary encoding: `[ver u8 = 1][family u8: 4 or 6]
[addr 4 or 16 bytes][port u16 little-endian][payload…]`. The previous
string-addressed encoding (`[u16 len]["ip:port"][payload]`) SHALL NOT be
produced or accepted by either side.

#### Scenario: Kernel encodes an inbound datagram
- **WHEN** the kernel UDP proxy receives a datagram from
  `203.0.113.7:5353` with payload P
- **THEN** it SHALL write a frame with `ver = 1`, `family = 4`, the four
  address octets, port `5353` little-endian, and payload P

#### Scenario: Guest encodes an outbound datagram
- **WHEN** a guest sends a `Datagram` to `[2001:db8::1]:443`
- **THEN** the frame SHALL carry `family = 6`, the sixteen address
  octets, and port `443`; the kernel proxy SHALL parse it without string
  conversion and emit the datagram

#### Scenario: Quinn adapter compatibility
- **WHEN** a future `QuinnUdpSocket` adapter maps `Transmit`/`RecvMeta`
  onto the ring format
- **THEN** the mapping SHALL be a direct binary `SocketAddr`
  encode/decode with no string allocation

### Requirement: UDP Datagram Receive Channel
The UDP receive channel (kernel→guest) SHALL carry incoming datagrams as
framed messages using the binary datagram frame format. Each frame SHALL
encode the source address and the raw datagram payload.

#### Scenario: Guest receives a datagram
- **WHEN** the kernel proxy writes an incoming datagram into the recv
  ring buffer and signals the guest
- **THEN** the guest SHALL be able to read the binary frame and extract
  both the source `SocketAddr` and the payload bytes

#### Scenario: Recv channel empty
- **WHEN** the guest attempts to read from an empty recv channel
- **THEN** the read SHALL return a `ChannelEmpty` indication and the
  guest SHALL wait on the recv signal for notification

### Requirement: UDP Datagram Send Channel
The UDP send channel (guest→kernel) SHALL carry outgoing datagrams as
framed messages using the binary datagram frame format. Each frame SHALL
encode the destination address and the raw datagram payload.

#### Scenario: Guest sends a datagram
- **WHEN** the guest writes a binary frame containing a destination
  `SocketAddr` and payload into the send ring buffer and notifies the
  send signal
- **THEN** the kernel proxy SHALL read the frame and call `sendto()` with
  the specified destination and payload

#### Scenario: Send channel full
- **WHEN** the guest attempts to write to a full send channel
- **THEN** the write SHALL return a `ChannelFull` indication and the
  guest SHALL wait on the send signal for space to become available

### Requirement: UDP Socket via Shared-Memory Rings
`selium-guest` SHALL implement datagram I/O over the shared-memory
send/recv rings created by the `UdpBind` hostcall, using the ring
buffer's atomic operations and generation-wait wakeups.

#### Scenario: Guest sends with full send ring
- **WHEN** the guest calls `poll_send` and the send ring is full
- **THEN** the implementation SHALL return `Poll::Pending` after
  registering a generation wait, and SHALL NOT spin

#### Scenario: Guest polls an empty recv ring
- **WHEN** the guest calls `poll_recv` and the recv ring is empty but
  writers are still connected
- **THEN** the implementation SHALL return `Poll::Pending` after
  registering a generation wait

#### Scenario: Guest polls a closed recv ring
- **WHEN** the guest calls `poll_recv` and the recv ring's
  `writer_count` is 0
- **THEN** the implementation SHALL return an error indicating the
  channel is closed

### Requirement: UDP Resource Lifecycle
`selium-kernel` SHALL track UDP socket state and clean up resources (OS socket, proxy threads, shared memory, signals) when the guest closes the socket or exits.

#### Scenario: Guest closes a UDP socket
- **WHEN** the `UdpSocket` handle is dropped or the guest process exits
- **THEN** the kernel SHALL stop the proxy threads, close the OS UDP socket, and release the shared memory region and signals

### Requirement: Capability-Gated UDP Access
`selium-abi` SHALL include `UdpSocket` in the `ResourceClass` enum so that UDP socket access can be controlled through the capability model.

#### Scenario: Guest denied UDP bind without capability
- **WHEN** a guest without the `Network` capability for `ResourceClass::UdpSocket` attempts to invoke `UdpBind`
- **THEN** the runtime SHALL deny the hostcall with an explicit permission error
