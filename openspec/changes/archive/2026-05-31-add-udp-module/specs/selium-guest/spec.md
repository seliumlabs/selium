## ADDED Requirements

### Requirement: UdpSocket Guest Handle
`selium-guest` SHALL provide a `net::udp::UdpSocket` type that wraps the `UdpBind` hostcall and exposes datagram send/receive operations through the shared-memory channel mechanism.

#### Scenario: Guest binds a UDP socket via the SDK
- **WHEN** guest code calls `UdpSocket::bind("0.0.0.0:8080").await`
- **THEN** the SDK SHALL invoke the `UdpBind` hostcall and return a configured `UdpSocket` handle backed by the recv and send ring buffers

#### Scenario: Guest sends a datagram
- **WHEN** guest code calls `udp_socket.send_to(b"hello", addr).await`
- **THEN** the SDK SHALL frame the destination address and payload into the send channel and await signal notification if the channel is full

#### Scenario: Guest receives a datagram
- **WHEN** guest code calls `udp_socket.recv_from(&mut buf).await`
- **THEN** the SDK SHALL read a frame from the recv channel and return the payload bytes and source address

#### Scenario: Guest checks local address
- **WHEN** guest code calls `udp_socket.local_addr()`
- **THEN** the SDK SHALL return the `SocketAddr` that the OS socket was bound to

### Requirement: UDP Module Export
`selium-guest` SHALL re-export `net::udp::UdpSocket` from the crate root so that guest code can access it without deep module paths.

#### Scenario: Guest imports UdpSocket from crate root
- **WHEN** guest code references `selium_guest::UdpSocket`
- **THEN** the type SHALL resolve to `selium_guest::net::udp::UdpSocket`
