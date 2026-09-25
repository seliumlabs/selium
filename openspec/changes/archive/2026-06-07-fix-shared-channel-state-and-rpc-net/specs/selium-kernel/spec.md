## MODIFIED Requirements

### Requirement: Protocol-Neutral Network Primitives
`selium-kernel` SHALL expose protocol-neutral listener, session, stream, and request/response network primitives. Network proxy threads SHALL coordinate with guests through the standard shared-memory ring buffer layout using atomic operations on the coordination fields in page 0.

#### Scenario: Guest opens outbound stream
- **WHEN** a guest with the required network capability opens an outbound stream
- **THEN** the kernel SHALL provide a stream resource backed by a shared region with the standard ring buffer layout, and spawn proxy threads that use `fetch_add`/`compare_exchange` on shared-memory atomics

#### Scenario: Guest opens a UDP socket
- **WHEN** a guest with the required network capability opens a UDP socket
- **THEN** the kernel SHALL provide a datagram socket resource backed by a shared region with the standard ring buffer layout, and spawn proxy threads that coordinate through shared-memory atomics

### Requirement: Shared Region Layout Header
`selium-kernel` shared memory regions SHALL support a layout header (magic, capacity, memory count, per-memory offset/length pairs) so that multiple parties can discover sub-memories after attaching via `shared_id`. Each sub-memory SHALL use the standard ring buffer coordination layout with generation counter, `next_tail`, `writer_count`, and `reader_slots` in page 0.

#### Scenario: Two guests attach the same region and agree on layout
- **WHEN** a guest seals a region built with `SharedRegionBuilder` and another guest attaches the same `shared_id`
- **THEN** both parties SHALL read the identical layout header and enumerate the same sub-memories, each with the standard coordination fields

### Requirement: UDP Bind Implementation
`selium-kernel` SHALL implement `Kernel::udp_bind(address: String) -> Result<SharedRegionDescriptor>` that binds a real OS UDP socket and creates the shared-memory channel infrastructure using the standard ring buffer layout.

#### Scenario: Kernel binds UDP socket
- **WHEN** `udp_bind` is called with a valid address
- **THEN** the kernel SHALL bind a `std::net::UdpSocket`, allocate a shared region with two ring buffers (recv and send) using the standard coordination layout, spawn proxy threads, and return a `SharedRegionDescriptor`

### Requirement: UDP Proxy Threads
`selium-kernel` SHALL spawn two OS threads per UDP socket: one for recv (kernel→guest) and one for send (guest→kernel). Each proxy thread SHALL coordinate through shared-memory atomics on the standard ring buffer layout, using `fetch_add`/`compare_exchange` on the shared `next_tail` and polling the generation counter for guest writes.

#### Scenario: Recv proxy forwards datagrams to guest
- **WHEN** the recv proxy thread receives a datagram via `recvfrom()`
- **THEN** it SHALL reserve space via CAS on the shared `next_tail`, write the source address and payload as a frame with single-phase write and release fencing, bump the generation counter, and notify via the wasmtiny Store

#### Scenario: Send proxy sends datagrams from guest
- **WHEN** the send proxy thread detects the generation counter has changed via polling
- **THEN** it SHALL read frames from the send ring buffer, extract the destination address and payload, and call `sendto()`

### Requirement: UDP Socket State Tracking
`selium-kernel` SHALL maintain `UdpSocketState` in `KernelInner` for lifecycle management and cleanup, using a shared `AtomicBool` for the running flag to coordinate proxy thread shutdown.

#### Scenario: Kernel tracks UDP socket state
- **WHEN** a UDP socket is created via `udp_bind`
- **THEN** the kernel SHALL insert a `UdpSocketState{ running }` into `KernelInner.udp_sockets` keyed by `shared_id`

### Requirement: UDP Socket Cleanup
`selium-kernel` SHALL provide `Kernel::close_udp_socket(shared_id)` that stops proxy threads, closes the OS socket, and releases resources.

#### Scenario: Process teardown closes UDP socket
- **WHEN** a guest process exits and the runtime cleans up resources with `ResourceClass::UdpSocket`
- **THEN** the kernel SHALL call `close_udp_socket`, which sets `running = false` to stop proxy threads and removes the state entry

## REMOVED Requirements

### Requirement: Explicit Signalling Primitive
**Reason**: The `Signal` type and its associated hostcalls (`signal_create`, `signal_wait`, `signal_notify`) were removed in the 2026-06-03 migration. Cross-process notification now uses the generation counter in the shared region with `memory.atomic.wait32`/`notify`.
**Migration**: All coordination that previously used `Signal` now uses atomic wait/notify on the shared region's generation counter. The kernel proxy polls the generation counter instead of waiting on a signal.
