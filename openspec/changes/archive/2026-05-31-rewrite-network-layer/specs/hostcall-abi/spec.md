## REMOVED Requirements

### Requirement: NetworkListen hostcall
**Reason**: Replaced by `TcpBind` which returns a `HostQueue` descriptor instead of `NetworkListenerDescriptor`. Connection acceptance is now handled through the `ResourceListener`/`HostQueue` mechanism.
**Migration**: Use `TcpListener::bind()` which internally calls `hostcall_async(TcpBind)` and returns a `ResourceListener`. Accept connections via `listener.accept::<TcpAccept>()`.

### Requirement: NetworkListenerClose hostcall
**Reason**: Listener lifecycle is now managed through `HostQueue` close semantics. Closing a listener closes the underlying `HostQueue`.
**Migration**: Close the `TcpListener` which closes the underlying `ResourceListener`.

### Requirement: NetworkConnect hostcall
**Reason**: Replaced by `TcpConnect` which returns a `SharedRegionDescriptor` containing ring buffers for zero-hostcall I/O, instead of `NetworkSessionDescriptor`.
**Migration**: Use `TcpStream::connect()` which internally calls `hostcall_async(TcpConnect)` and attaches to the returned shared region.

### Requirement: NetworkSessionClose hostcall
**Reason**: Sessions no longer exist. Connection lifecycle is managed through the `TcpStream` drop semantics (writer count decrements).
**Migration**: Drop the `TcpStream`; the kernel proxy detects writer count reaching 0 and closes the real socket.

### Requirement: NetworkOpenStream hostcall
**Reason**: Streams are no longer opened within sessions. `TcpStream` directly attaches to a shared-memory region with two ring buffers.
**Migration**: Use `TcpStream::connect()` or `TcpStream::attach_shared(shared_id)` directly.

### Requirement: NetworkStreamSend hostcall
**Reason**: I/O operations now use shared-memory ring buffers with zero hostcalls per operation. Writing goes through `StrongWriter` on the outbound ring buffer.
**Migration**: Use `tokio::io::AsyncWrite::poll_write` on `TcpStream`, which writes to the shared-memory ring buffer directly.

### Requirement: NetworkStreamRecv hostcall
**Reason**: I/O operations now use shared-memory ring buffers with zero hostcalls per operation. Reading goes through `StrongReader` on the inbound ring buffer with signal-based notification.
**Migration**: Use `tokio::io::AsyncRead::poll_read` on `TcpStream`, which reads from the shared-memory ring buffer directly.

### Requirement: NetworkStreamClose hostcall
**Reason**: Stream lifecycle is managed through shared-memory writer count semantics. Dropping the `TcpStream` decrements the outbound writer count.
**Migration**: Drop the `TcpStream`; the kernel proxy detects closure via writer count semantics.

### Requirement: NetworkSendRequest hostcall
**Reason**: HTTP request/response handling is no longer a kernel responsibility. Use axum or hyper on top of `TcpStream` for HTTP semantics.
**Migration**: Use `TcpStream` with an HTTP library (e.g., hyper via axum integration) for request/response patterns.

### Requirement: NetworkWaitRequestResponse hostcall
**Reason**: HTTP request/response handling is no longer a kernel responsibility. Use axum or hyper on top of `TcpStream` for HTTP semantics.
**Migration**: Use `TcpStream` with an HTTP library for request/response patterns.

## REMOVED Types

### Requirement: NetworkListenerDescriptor type
**Reason**: Replaced by `HostQueueDescriptor` returned from `TcpBind`. The listener is now a `ResourceListener` wrapping a host queue.
**Migration**: Use `HostQueueDescriptor` (from `HostcallOutput::HostQueue`) directly.

### Requirement: NetworkSessionDescriptor type
**Reason**: Sessions no longer exist as a concept. Outbound connections use `SharedRegionDescriptor` returned from `TcpConnect`.
**Migration**: Use `SharedRegionDescriptor` for outbound connections.

### Requirement: NetworkStreamDescriptor type
**Reason**: Streams no longer exist as separate kernel resources. `TcpStream` operates on shared-memory ring buffers.
**Migration**: No replacement needed; `TcpStream` manages its own ring buffer attachment.

### Requirement: RequestExchange type
**Reason**: Request/response exchange is no longer a kernel primitive. HTTP and RPC patterns are built on top of `TcpStream` or existing `RpcConnection`.
**Migration**: Use `TcpStream` with an HTTP library, or use `RpcConnection`/`RpcClient` for typed request/response between guests.

## MODIFIED Requirements

### Requirement: ResourceClass enum

The `ResourceClass` enum SHALL have its `Listener`, `Session`, `Stream`, and `RequestExchange` variants removed and replaced with `TcpListener` and `TcpStream` variants. The `TcpListener` resource class represents a bound TCP listener managed through a HostQueue. The `TcpStream` resource class represents an active TCP connection backed by shared-memory ring buffers.

#### Scenario: Capability check for TCP bind

- **WHEN** a guest requests `Capability::Network` with `ResourceClass::TcpListener`
- **THEN** the kernel checks whether the guest is permitted to bind TCP listeners

#### Scenario: Capability check for TCP connect

- **WHEN** a guest requests `Capability::Network` with `ResourceClass::TcpStream`
- **THEN** the kernel checks whether the guest is permitted to establish outbound TCP connections

## ADDED Requirements

### Requirement: TcpBind hostcall

`HostcallRequest::TcpBind { address: String }` SHALL instruct the kernel to bind a real TCP socket at the given address, create a `HostQueue` for accepting connections, and start an accept loop. The kernel SHALL return `HostcallOutput::HostQueue(HostQueueDescriptor)`.

#### Scenario: Successful TCP bind

- **WHEN** `TcpBind { address: "127.0.0.1:443" }` is issued
- **THEN** the kernel binds a `tokio::net::TcpListener`, creates a `HostQueue`, starts an accept loop, and returns `HostQueue(descriptor)`

#### Scenario: TCP bind with invalid address

- **WHEN** `TcpBind { address: "invalid" }` is issued
- **THEN** the kernel returns an error

### Requirement: TcpConnect hostcall

`HostcallRequest::TcpConnect { address: String }` SHALL instruct the kernel to open a real TCP connection to the given address, create a shared-memory region with two ring buffers and two signals, start a bidirectional proxy task, and return `HostcallOutput::SharedRegion(SharedRegionDescriptor)`. The guest then attaches to this region via `TcpStream::attach_shared`.

#### Scenario: Successful TCP connect

- **WHEN** `TcpConnect { address: "127.0.0.1:8080" }` is issued
- **THEN** the kernel establishes a real TCP connection, creates a shared region with 2 sub-memories (inbound/outbound ring buffers) and 2 signals, starts a proxy task, and returns `SharedRegion(descriptor)`

#### Scenario: TCP connect to unreachable host

- **WHEN** `TcpConnect { address: "192.0.2.1:80" }` is issued and the host is unreachable
- **THEN** the kernel returns a connection error