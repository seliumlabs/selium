## ADDED Requirements

### Requirement: UdpBind Hostcall
`selium-abi` SHALL define a `UdpBind` variant on `HostcallRequest` with an `address: String` field, and the hostcall SHALL return a `SharedRegion` output containing the UDP socket's shared-memory channels.

#### Scenario: UdpBind hostcall round-trip
- **WHEN** a guest encodes a `HostcallRequest::UdpBind { address: "0.0.0.0:0" }` and the kernel processes it
- **THEN** the hostcall SHALL complete with `HostcallOutput::SharedRegion(descriptor)` where `descriptor.shared_id` identifies a region containing the recv and send ring buffers

### Requirement: UdpSocket Resource Class
`selium-abi` SHALL add `UdpSocket` to the `ResourceClass` enum for capability-gated access control.

#### Scenario: Capability check includes UdpSocket
- **WHEN** the runtime evaluates a capability grant for a `UdpBind` operation
- **THEN** the scope context SHALL contain `ResourceClass::UdpSocket` and the grant SHALL be evaluated against that class
