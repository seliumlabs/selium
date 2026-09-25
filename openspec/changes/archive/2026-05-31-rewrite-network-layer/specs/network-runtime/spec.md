## ADDED Requirements

### Requirement: Kernel creates shared regions for TCP connections

On both `TcpBind` (accept) and `TcpConnect` (outbound), the kernel SHALL create a `SharedRegion` containing exactly 2 sub-memories. Each sub-memory SHALL be initialised as a ring buffer with a dedicated `Signal`. Channel 0 is inbound (kernel writes, guest reads). Channel 1 is outbound (guest writes, kernel reads). The region layout SHALL follow the same `SharedRegionBuilder` format used by `RpcClient::connect`.

#### Scenario: Inbound connection region creation

- **WHEN** the kernel accepts a real TCP connection on a bound listener
- **THEN** the kernel allocates a `SharedRegion` with 2 sub-memories of the configured capacity, creates 2 `Signal`s, initialises each sub-memory as a ring buffer via the kernel-side equivalent of `ChannelRegion::initialise`, and starts a proxy task

#### Scenario: Outbound connection region creation

- **WHEN** a guest calls `TcpConnect`
- **THEN** the kernel opens a real TCP connection, creates the same 2-channel shared region, initialises ring buffers, starts a proxy task, and returns `HostcallOutput::SharedRegion(descriptor)`

### Requirement: Kernel enqueues accepted connections into HostQueue

For `TcpBind`, the kernel SHALL create a `HostQueue` and return its `HostQueueDescriptor`. When a real TCP connection is accepted, the kernel SHALL enqueue a `ConnectionInfo { client_process_id: 0, value: shared_id }` into this queue. The `shared_id` identifies the shared region containing the connection's ring buffers.

#### Scenario: Accepted connection enqueued

- **WHEN** a real TCP connection is accepted on a bound listener
- **THEN** `{ client_process_id: 0, value: shared_id }` is enqueued into the listener's `HostQueue`
- **AND** the guest's `ResourceListener::recv()` call returns `IncomingConnection { shared_id, client_process_id: 0 }`

### Requirement: Kernel proxy task forwards bytes bidirectionally

Each active TCP connection SHALL have a kernel-side tokio task that proxies bytes between the real TCP socket and the shared-memory ring buffers. The proxy SHALL use `tokio::select!` to concurrently: (1) read from the real socket and write to the inbound ring buffer, then notify the inbound signal; (2) wait on the outbound signal and read from the outbound ring buffer, then write to the real socket.

#### Scenario: Kernel reads from real socket and writes to inbound ring

- **WHEN** data arrives on the real TCP socket
- **THEN** the proxy reads it into a buffer, writes it to the inbound ring buffer, and notifies the inbound signal so the guest can read it

#### Scenario: Kernel reads from outbound ring and writes to real socket

- **WHEN** the outbound signal is notified (guest wrote data)
- **THEN** the proxy reads data from the outbound ring buffer and writes it to the real TCP socket

### Requirement: Kernel proxy detects guest close

When the guest's outbound ring writer count reaches 0, the kernel proxy SHALL shut down the write side of the real TCP socket. When the real TCP socket reaches EOF, the kernel proxy SHALL decrement the inbound ring's writer count and stop the inbound proxy loop.

#### Scenario: Guest closes connection

- **WHEN** the guest drops a `TcpStream`, decrementing the outbound writer count to 0
- **THEN** the kernel proxy detects `writer_count == 0` and shuts down the write side of the real socket
- **AND** after the real socket finishes draining, the kernel proxy decrements the inbound writer count
- **AND** the guest's next `poll_read` returns `Ok(0)` (EOF)

#### Scenario: Remote peer closes connection

- **WHEN** the remote TCP peer closes the connection
- **THEN** the kernel proxy reads `Ok(0)` from the real socket
- **AND** the proxy decrements the inbound ring's writer count
- **AND** notifies the inbound signal
- **AND** the guest's `poll_read` returns `Ok(0)` (EOF)

### Requirement: Kernel proxy handles backpressure

If the inbound ring buffer is full (the guest is reading slowly), the kernel proxy SHALL stop reading from the real TCP socket until the guest catches up. If the outbound ring buffer is full (the guest is writing faster than the remote can consume), the guest's `poll_write` returns `Poll::Pending` and the kernel proxy continues writing to the real socket until buffer space is freed.

#### Scenario: Inbound backpressure

- **WHEN** the inbound ring buffer is full
- **THEN** the kernel proxy stops reading from the real TCP socket until the guest reads data and frees space in the ring

#### Scenario: Outbound backpressure

- **WHEN** the outbound ring buffer is full and the guest tries to write
- **THEN** the guest's `poll_write` returns `Poll::Pending`
- **AND** the kernel proxy continues draining the outbound ring to the real socket, freeing space

### Requirement: Kernel ring buffer primitives mirror guest-side operations

The kernel SHALL provide ring buffer read/write operations that operate directly on shared memory (using `read_shared_memory`, `write_shared_memory`, `fetch_add_shared_memory_u64`, `compare_exchange_shared_memory_u64`) without going through hostcalls. These SHALL implement the same protocol as the guest-side `RingBuf` but from the host perspective.

#### Scenario: Kernel writes to inbound ring atomically

- **WHEN** the kernel proxy writes TCP data to the inbound ring
- **THEN** it reserves tail space atomically, writes data, and advances the tail — mirroring `StrongWriter::write`

#### Scenario: Kernel reads from outbound ring atomically

- **WHEN** the kernel proxy reads data from the outbound ring
- **THEN** it reads frames atomically, advancing its reader cursor — mirroring `StrongReader::read`