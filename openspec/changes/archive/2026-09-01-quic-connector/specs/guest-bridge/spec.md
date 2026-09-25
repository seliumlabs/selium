## MODIFIED Requirements

### Requirement: Bridge Is Deployable Guest Code
The bridge SHALL be implemented as a standard WASM guest using `selium-guest`. It SHALL NOT depend on the deleted `selium-quic` crate; QUIC termination SHALL instead be provided by the QUIC connector (`quic-connector`), which the frozen bridge reuses or replaces if it is re-activated. The bridge SHALL NOT require special runtime modifications beyond the existing UDP datagram hostcall.

#### Scenario: Bridge deployed via normal guest lifecycle
- **WHEN** the platform starts a bridge guest via `Process::start` with appropriate capability grants
- **THEN** the bridge SHALL initialize QUIC on the granted UDP socket, accept incoming streams, and relay frames to/from channels within its grants
