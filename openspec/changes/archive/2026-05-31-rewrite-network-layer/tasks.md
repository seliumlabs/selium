## 1. ABI Changes (selium-abi)

- [x] 1.1 Add `TcpBind { address: String }` and `TcpConnect { address: String }` variants to `HostcallRequest`
- [x] 1.2 Remove `NetworkListen`, `NetworkListenerClose`, `NetworkConnect`, `NetworkSessionClose`, `NetworkOpenStream`, `NetworkStreamSend`, `NetworkStreamRecv`, `NetworkStreamClose`, `NetworkSendRequest`, `NetworkWaitRequestResponse` variants from `HostcallRequest`
- [x] 1.3 Remove `Listener`, `Session`, `Stream` variants from `ResourceClass`; add `TcpListener`, `TcpStream` variants
- [x] 1.4 Remove `NetworkListenerDescriptor`, `NetworkSessionDescriptor`, `NetworkStreamDescriptor` types from selium-abi
- [x] 1.5 Ensure `HostcallOutput::SharedRegion` and `HostcallOutput::HostQueue` remain unchanged (they carry TcpConnect and TcpBind results)
- [x] 1.6 Update ABI round-trip tests to cover `TcpBind` and `TcpConnect` request variants

## 2. Guest-Side TcpStream (selium-guest)

- [x] 2.1 Create `crates/core/guest/src/tcp.rs` with `TcpStream` struct holding `StrongReader`, `StrongWriter`, and inbound/outbound `Signal`s
- [x] 2.2 Implement `TcpStream::connect(address: impl Into<String>) -> Result<Self>` using `hostcall_async(TcpConnect)` and internal `attach_tcp_channels`
- [x] 2.3 Implement `TcpStream::attach_shared(shared_id: u64) -> Result<Self>` that maps the 2-channel shared region, wraps ring buffers, sets up reader/writer
- [x] 2.4 Implement `tokio::io::AsyncRead` for `TcpStream` using inbound `StrongReader` with signal-based pending on `ChannelEmpty`
- [x] 2.5 Implement `tokio::io::AsyncWrite` for `TcpStream` using outbound `StrongWriter` with auto-signal notification
- [x] 2.6 Implement `poll_flush` as immediate `Poll::Ready(Ok(()))`
- [x] 2.7 Implement `poll_shutdown` by decrementing outbound writer count
- [x] 2.8 Add `attach_tcp_channels(shared_id)` helper (mirror of `attach_rpc_channels`) for mapping 2-channel shared regions

## 3. Guest-Side TcpListener and TcpAccept (selium-guest)

- [x] 3.1 Create `TcpListener` struct wrapping `ResourceListener` and storing `SocketAddr`
- [x] 3.2 Implement `TcpListener::bind(address: impl Into<String>) -> Result<Self>` using `hostcall_async(TcpBind)` and `ResourceListener::from_queue(descriptor)`
- [x] 3.3 Implement `TcpListener::accept(&self) -> impl Future<Output = Result<TcpStream>>` delegating to `self.listener.accept::<TcpAccept>()`
- [x] 3.4 Implement `TcpListener::local_addr(&self) -> SocketAddr`
- [x] 3.5 Implement `TcpAccept` with `type Item = TcpStream` and `Accept::accept(IncomingConnection) -> Result<TcpStream>` calling `TcpStream::attach_shared`
- [x] 3.6 Add `network` module with `TcpListener`, `TcpStream`, `TcpAccept` to `lib.rs` public exports

## 4. axum Integration (selium-guest, feature-gated)

- [x] 4.1 Add `axum` feature flag to `selium-guest/Cargo.toml` with `axum` and `tokio` dependencies
- [x] 4.2 Implement `axum::serve::Listener for TcpListener` with `type Io = TcpStream` and `type Addr = SocketAddr`
- [x] 4.3 Implement `Listener::accept` bridging to async `TcpListener::accept`
- [x] 4.4 Implement `Listener::local_addr` delegating to `TcpListener::local_addr`

## 5. Kernel Network Runtime (selium-kernel)

- [x] 5.1 Add `tokio` dependency with `net` feature to `selium-kernel/Cargo.toml`
- [x] 5.2 Create `crates/core/kernel/src/network_runtime.rs` with TCP listener and proxy task infrastructure
- [x] 5.3 Implement `TcpBind` hostcall handler: bind `std::net::TcpListener`, create `HostQueue`, spawn accept loop, return `HostQueueDescriptor`
- [x] 5.4 Implement `TcpConnect` hostcall handler: open `std::net::TcpStream`, create shared region + ring buffers + signals, spawn proxy task, return `SharedRegionDescriptor`
- [x] 5.5 Implement bidirectional proxy task: std::thread proxies between real socket and ring buffers
- [x] 5.6 Implement kernel-side ring buffer write (inbound): atomically reserve tail, write framed data, notify inbound signal
- [x] 5.7 Implement kernel-side ring buffer read (outbound): read frames, advance reader cursor, handle empty ring by polling
- [x] 5.8 Implement close detection: decrement writer count on inbound ring when real socket reaches EOF; detect guest close via outbound ring writer count == 0
- [x] 5.9 Implement backpressure: when inbound ring is full, stop reading from real socket until guest reads; when outbound ring is empty, wait on signal

## 6. Remove Legacy Network Code

- [x] 6.1 Remove `crates/core/guest/src/network.rs` (`NetworkListener`, `NetworkSession`, `NetworkStream`, `RequestExchange`)
- [x] 6.2 Remove `crates/core/kernel/src/network.rs` (old network hostcall handlers, `ListenerState`, `SessionState`, `StreamState`, `RequestExchangeState`)
- [x] 6.3 Remove old network-related fields from `KernelInner` in `crates/core/kernel/src/state.rs`
- [x] 6.4 Remove old network hostcall handlers from the runtime dispatch
- [x] 6.5 Add `TcpBind` and `TcpConnect` dispatch in the runtime hostcall handler

## 7. Testing

- [x] 7.1 Write unit tests for `TcpStream::attach_shared` with invalid regions
- [x] 7.2 Write unit tests for `TcpAccept::accept` producing `TcpStream` from `IncomingConnection`
- [x] 7.3 Write kernel integration test: bind listener, accept connection, verify HostQueue enqueue
- [x] 7.4 Write kernel integration test: connect outbound, verify SharedRegion return
- [x] 7.5 Write end-to-end test: guest binds listener, guest connects, bidirectional byte echo
- [x] 7.6 Write end-to-end test: EOF propagation in both directions
- [x] 7.7 Write test: backpressure when inbound ring is full (kernel stops reading)
- [x] 7.8 Write test: axum Listener integration with simple HTTP GET
