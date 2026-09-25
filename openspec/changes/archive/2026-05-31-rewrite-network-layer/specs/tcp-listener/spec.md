## ADDED Requirements

### Requirement: TcpListener binds a real TCP socket via the kernel

`TcpListener` SHALL provide an async `bind` method that takes a socket address, calls `hostcall_async(TcpBind { address })`, and returns a `TcpListener` wrapping a `ResourceListener`. The kernel SHALL bind a real `tokio::net::TcpListener` and create a `HostQueue` for incoming connections. The hostcall SHALL return `HostcallOutput::HostQueue(HostQueueDescriptor)`.

#### Scenario: Successful bind

- **WHEN** `TcpListener::bind("127.0.0.1:443")` is called
- **THEN** a `hostcall_async(TcpBind { address: "127.0.0.1:443" })` is dispatched
- **AND** on success, a `TcpListener` is returned containing a `ResourceListener` built from the returned `HostQueueDescriptor`

#### Scenario: Bind fails with invalid address

- **WHEN** `TcpListener::bind("invalid")` is called
- **THEN** the hostcall returns an error indicating the bind failed

### Requirement: TcpListener accepts connections via ResourceListener

`TcpListener` SHALL delegate `accept()` to `ResourceListener::accept::<TcpAccept>()`. The kernel SHALL enqueue an `IncomingConnection { client_process_id: 0, shared_id }` into the HostQueue for each accepted real TCP connection, where `shared_id` identifies the shared-memory region containing the connection's ring buffers.

#### Scenario: Accept returns a TcpStream

- **WHEN** `listener.accept().await` is called
- **THEN** `ResourceListener::recv()` is called, which calls `hostcall_async(HostQueueRecv { local_id })`
- **AND** the kernel dequeues an `IncomingConnection` with `shared_id` pointing to a two-channel shared region
- **AND** `TcpAccept::accept(connection)` returns `TcpStream::attach_shared(connection.shared_id)`

### Requirement: TcpAccept implements the Accept trait

`TcpAccept` SHALL implement the `Accept` trait with `type Item = TcpStream`. Its `accept` method SHALL call `TcpStream::attach_shared` with the `shared_id` from the `IncomingConnection`.

#### Scenario: Accept produces a TcpStream from an incoming connection

- **WHEN** `TcpAccept::accept(IncomingConnection { shared_id, client_process_id: 0 })` is called
- **THEN** `TcpStream::attach_shared(shared_id)` is called and the resulting `TcpStream` is returned

### Requirement: TcpListener provides local_addr

`TcpListener` SHALL store the bound address and return it via `local_addr()`. The address SHALL be the string passed to `bind`.

#### Scenario: local_addr returns the bound address

- **WHEN** `listener.local_addr()` is called on a listener bound to "127.0.0.1:443"
- **THEN** the corresponding `std::net::SocketAddr` is returned

### Requirement: TcpListener implements axum Listener when axum feature is enabled

When the `axum` feature is enabled, `TcpListener` SHALL implement `axum::serve::Listener` with `type Io = TcpStream` and `type Addr = std::net::SocketAddr`. The `accept` method SHALL delegate to the async `TcpListener::accept` and convert errors.

#### Scenario: axum serve with TcpListener

- **WHEN** `axum::serve(axum_router, tcp_listener)` is called
- **THEN** axum accepts incoming connections as `TcpStream` and processes HTTP requests over the shared-memory ring buffers