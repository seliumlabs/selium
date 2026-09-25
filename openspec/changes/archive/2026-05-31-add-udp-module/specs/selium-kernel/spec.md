## ADDED Requirements

### Requirement: UDP Bind Implementation
`selium-kernel` SHALL implement `Kernel::udp_bind(address: String) -> Result<SharedRegionDescriptor>` that binds a real OS UDP socket and creates the shared-memory channel infrastructure.

#### Scenario: Kernel binds UDP socket
- **WHEN** `udp_bind` is called with a valid address
- **THEN** the kernel SHALL bind a `std::net::UdpSocket`, allocate a shared region with two ring buffers (recv and send) and two signals, spawn proxy threads, and return a `SharedRegionDescriptor`

### Requirement: UDP Proxy Threads
`selium-kernel` SHALL spawn two OS threads per UDP socket: one for recv (kernel→guest) and one for send (guest→kernel), following the same OS-thread-per-direction pattern as the TCP proxy.

#### Scenario: Recv proxy forwards datagrams to guest
- **WHEN** the recv proxy thread receives a datagram via `recvfrom()`
- **THEN** it SHALL write the source address and payload into the recv ring buffer and notify the recv signal

#### Scenario: Send proxy sends datagrams from guest
- **WHEN** the send proxy thread reads a frame from the send ring buffer
- **THEN** it SHALL call `sendto()` with the destination address and payload extracted from the frame

### Requirement: UDP Socket State Tracking
`selium-kernel` SHALL maintain `UdpSocketState` in `KernelInner` for lifecycle management and cleanup.

#### Scenario: Kernel tracks UDP socket state
- **WHEN** a UDP socket is created via `udp_bind`
- **THEN** the kernel SHALL insert a `UdpSocketState{ running, recv_signal, send_signal }` into `KernelInner.udp_sockets` keyed by `shared_id`

### Requirement: UDP Socket Cleanup
`selium-kernel` SHALL provide `Kernel::close_udp_socket(shared_id)` that stops proxy threads, closes the OS socket, and releases resources.

#### Scenario: Process teardown closes UDP socket
- **WHEN** a guest process exits and the runtime cleans up resources with `ResourceClass::UdpSocket`
- **THEN** the kernel SHALL call `close_udp_socket`, which sets `running = false`, notifies both signals to unblock proxy threads, and removes the state entry

### Requirement: Protocol-Neutral Network Primitives Extension
The existing requirement for protocol-neutral network primitives SHALL be extended to include datagram-oriented (UDP) sockets alongside the existing stream-oriented (TCP) primitives.

#### Scenario: Guest opens a UDP socket
- **WHEN** a guest with the required network capability opens a UDP socket
- **THEN** the kernel SHALL provide a datagram socket resource without embedding higher-level messaging semantics into the primitive
