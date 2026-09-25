## ADDED Requirements

### Requirement: TcpStream connects to a remote address via the kernel

`TcpStream` SHALL provide an async `connect` method that takes a socket address, calls `hostcall_async(TcpConnect { address })`, and returns a `TcpStream`. The kernel SHALL open a real TCP connection, create a shared-memory region with two ring buffers and two signals, start a proxy task, and return `HostcallOutput::SharedRegion(SharedRegionDescriptor)`. The guest SHALL internally call `TcpStream::attach_shared(descriptor.shared_id)` to set up ring buffer access.

#### Scenario: Successful outbound connection

- **WHEN** `TcpStream::connect("127.0.0.1:8080")` is called
- **THEN** `hostcall_async(TcpConnect { address: "127.0.0.1:8080" })` is dispatched
- **AND** on success, the kernel establishes a real TCP connection, creates a shared region with inbound and outbound ring buffers, starts a proxy task, and returns `HostcallOutput::SharedRegion(descriptor)`
- **AND** `TcpStream::attach_shared(descriptor.shared_id)` maps the ring buffers and returns a `TcpStream`

#### Scenario: Connection refused

- **WHEN** `TcpStream::connect("127.0.0.1:1")` is called and the remote refuses
- **THEN** the hostcall returns an error indicating connection failure

### Requirement: TcpStream attaches to an existing shared region

`TcpStream::attach_shared(shared_id)` SHALL parse the shared region layout (validating it has exactly 2 sub-memories), map the inbound and outbound sub-memories as `ChannelRegion`s with `RingBuf`s, attach to their signals, and construct a `StrongReader` on the inbound channel and a `StrongWriter` on the outbound channel. This SHALL follow the same pattern as `attach_rpc_channels`.

#### Scenario: Attach to a connection's shared region

- **WHEN** `TcpStream::attach_shared(shared_id)` is called with a valid shared region containing 2 sub-memories
- **THEN** the region header is read, sub-memory offsets and lengths are extracted, both sub-memories are mapped, ring buffers are initialised via `RingBuf::wrap_region`, and a `TcpStream` is returned with a `StrongReader` (inbound) and `StrongWriter` (outbound)

#### Scenario: Attach with invalid shared region

- **WHEN** `TcpStream::attach_shared(invalid_id)` is called with an invalid or corrupted shared region
- **THEN** an appropriate error is returned (invalid magic, wrong memory count, etc.)

### Requirement: TcpStream implements AsyncRead

`TcpStream` SHALL implement `tokio::io::AsyncRead`. The `poll_read` method SHALL attempt to read from the inbound `StrongReader`. If `ChannelEmpty` is returned, it SHALL check the writer count; if zero, it SHALL return `Ok(0)` (EOF). Otherwise, it SHALL wait on the inbound signal and return `Poll::Pending`.

#### Scenario: Read available data

- **WHEN** data is available in the inbound ring buffer
- **THEN** `poll_read` returns the data immediately as `Poll::Ready(Ok(n))`

#### Scenario: Read blocks when buffer is empty

- **WHEN** the inbound ring buffer is empty but the connection is still open
- **THEN** `poll_read` returns `Poll::Pending` and the guest task is woken when the inbound signal is notified

#### Scenario: Read returns EOF on connection close

- **WHEN** the kernel proxy has closed (writer count on inbound ring is 0)
- **THEN** `poll_read` returns `Poll::Ready(Ok(0))` indicating EOF

### Requirement: TcpStream implements AsyncWrite

`TcpStream` SHALL implement `tokio::io::AsyncWrite`. The `poll_write` method SHALL write bytes to the outbound `StrongWriter`. The `StrongWriter` SHALL auto-notify the outbound signal when data is written, waking the kernel proxy. If the buffer is full, `poll_write` SHALL return `Poll::Pending`.

#### Scenario: Write data successfully

- **WHEN** the outbound ring buffer has capacity
- **THEN** `poll_write` writes data to the ring buffer, the outbound signal is notified, and `Poll::Ready(Ok(n))` is returned

#### Scenario: Write blocks when buffer is full

- **WHEN** the outbound ring buffer is full
- **THEN** `poll_write` returns `Poll::Pending`

#### Scenario: Write fails on closed connection

- **WHEN** the connection has been closed or the ring buffer is in an error state
- **THEN** `poll_write` returns `Poll::Ready(Err(...))`

### Requirement: TcpStream flush is a no-op

`TcpStream::poll_flush` SHALL return `Poll::Ready(Ok(()))` immediately. The `StrongWriter` writes to shared memory synchronously — there is no kernel buffer to flush. The kernel proxy reads from the ring buffer on its own signal-driven schedule.

#### Scenario: Flush completes immediately

- **WHEN** `poll_flush` is called
- **THEN** `Poll::Ready(Ok(()))` is returned immediately

### Requirement: TcpStream shutdown closes the outbound channel

`TcpStream::poll_shutdown` SHALL decrement the outbound ring's writer count and return `Poll::Ready(Ok(()))`. This signals to the kernel proxy that no more data will be written, causing it to shut down the write side of the real TCP socket.

#### Scenario: Shutdown signals close to kernel

- **WHEN** `poll_shutdown` is called on a `TcpStream`
- **THEN** the outbound ring's writer count is decremented
- **AND** the kernel proxy detects `writer_count == 0` and shuts down the write side of the real socket