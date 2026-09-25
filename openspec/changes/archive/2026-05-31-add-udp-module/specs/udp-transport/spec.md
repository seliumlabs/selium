## ADDED Requirements

### Requirement: UDP Socket Binding via Hostcall
`selium-abi` SHALL define a `UdpBind` hostcall variant that accepts a socket address and returns a `SharedRegionDescriptor` containing the ring buffers and signal metadata for UDP communication.

#### Scenario: Guest binds a UDP socket
- **WHEN** a guest invokes `HostcallRequest::UdpBind` with a valid local address
- **THEN** the host SHALL bind a real OS UDP socket at that address and return a `SharedRegionDescriptor` referencing a shared memory region with two ring buffers (recv and send) and their associated signal ids embedded in the region headers

#### Scenario: Guest binds with an invalid address
- **WHEN** a guest invokes `UdpBind` with an address that cannot be bound (e.g. port in use, invalid address format)
- **THEN** the hostcall SHALL return an explicit `AbiError` with a descriptive message

### Requirement: UDP Datagram Receive Channel
The UDP receive channel (kernel→guest) SHALL carry incoming datagrams as framed messages. Each frame SHALL encode the source address and the raw datagram payload.

#### Scenario: Guest receives a datagram
- **WHEN** the kernel proxy writes an incoming datagram into the recv ring buffer and signals the guest
- **THEN** the guest SHALL be able to read the frame and extract both the source `SocketAddr` and the payload bytes

#### Scenario: Recv channel empty
- **WHEN** the guest attempts to read from an empty recv channel
- **THEN** the read SHALL return a `ChannelEmpty` indication and the guest SHALL wait on the recv signal for notification

### Requirement: UDP Datagram Send Channel
The UDP send channel (guest→kernel) SHALL carry outgoing datagrams as framed messages. Each frame SHALL encode the destination address and the raw datagram payload.

#### Scenario: Guest sends a datagram
- **WHEN** the guest writes a frame containing a destination `SocketAddr` and payload into the send ring buffer and notifies the send signal
- **THEN** the kernel proxy SHALL read the frame and call `sendto()` with the specified destination and payload

#### Scenario: Send channel full
- **WHEN** the guest attempts to write to a full send channel
- **THEN** the write SHALL return a `ChannelFull` indication and the guest SHALL wait on the send signal for space to become available

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
