## MODIFIED Requirements

### Requirement: UdpBind Hostcall (RETAINED AS STUB)
**Status**: `UdpBind` graduates from stub to fully implemented.
**Reason**: The guest networking reimplementation against the new shared-memory ring buffer layout provides a working UDP data plane, replacing the previous stub that always returned an error.
**Migration**: Existing code that matched on the error return from `UdpBind` must be updated to handle the successful `HostcallOutput::SharedRegion` response. The hostcall signature is unchanged.

### Requirement: TcpConnect Hostcall
`TcpConnect` SHALL return a `SharedRegionDescriptor` containing a multi-memory region with two ring buffers (inbound and outbound), initialised with the standard coordination layout.

#### Scenario: Guest connects to TCP endpoint
- **WHEN** a guest invokes `TcpConnect` with a valid address
- **THEN** the host SHALL create a TCP connection, allocate a shared region with two ring buffers using the standard layout, spawn proxy threads, and return the region descriptor

### Requirement: TcpBind Hostcall
`TcpBind` SHALL return a `HostQueueDescriptor` as before, with the kernel spawning an accept loop that creates per-connection shared regions using the standard ring buffer layout.

#### Scenario: Guest binds TCP listener
- **WHEN** a guest invokes `TcpBind` with a valid address
- **THEN** the host SHALL bind a TCP listener, create a host queue, spawn an accept loop, and return the queue descriptor

### Requirement: UdpBind Hostcall
`UdpBind` SHALL return a `SharedRegionDescriptor` containing a multi-memory region with two ring buffers (recv and send), initialised with the standard coordination layout.

#### Scenario: Guest binds UDP socket
- **WHEN** a guest invokes `UdpBind` with a valid address
- **THEN** the host SHALL bind a UDP socket, allocate a shared region with two ring buffers using the standard layout, spawn proxy threads, and return the region descriptor

## REMOVED Requirements

### Requirement: Host-Mediated Connection Queue Hostcalls (MODIFIED)
**Status**: Retained. `HostQueueCreate`, `HostQueueAttach`, `HostQueueSend`, and `HostQueueRecv` remain in the core ABI unchanged.
**Reason**: Host queues remain necessary for the TCP listener accept pattern and the RPC connection handshake. The previous spec incorrectly claimed RPC handoff had moved entirely to the shared memory ABI; in practice, the initial connection setup still uses `HostQueueSend`/`HostQueueRecv` to pass the `shared_id` from client to server.
