## MODIFIED Requirements

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
