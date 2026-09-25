## 1. ABI Changes

- [x] 1.1 Add `UdpSocket` variant to `ResourceClass` enum in `crates/core/abi/src/lib.rs`
- [x] 1.2 Add `UdpBind { address: String }` variant to `HostcallRequest` enum in `crates/core/abi/src/lib.rs`

## 2. Kernel UDP Runtime

- [x] 2.1 Add `UdpSocketState` struct to `crates/core/kernel/src/state.rs` with `running: Arc<AtomicBool>`, `recv_signal: Arc<SignalState>`, `send_signal: Arc<SignalState>`
- [x] 2.2 Add `udp_sockets: Mutex<HashMap<SharedResourceId, UdpSocketState>>` to `KernelInner` in `state.rs`
- [x] 2.3 Implement `Kernel::udp_bind(address: String)` in `crates/core/kernel/src/network_runtime.rs` that binds a real UDP socket, creates a shared region with 2 ring buffers, creates 2 signals, inserts state, and spawns proxy threads
- [x] 2.4 Implement `udp_proxy_recv` thread function that reads from the OS UDP socket via `recvfrom()` and writes framed datagrams into the guest's recv ring buffer
- [x] 2.5 Implement `udp_proxy_send` thread function that reads framed datagrams from the guest's send ring buffer and writes them to the OS UDP socket via `sendto()`
- [x] 2.6 Implement `Kernel::close_udp_socket(shared_id)` that stops proxy threads, closes the OS socket, and removes state
- [x] 2.7 Wire up `close_udp_socket` in `udp_sockets` cleanup on `KernelInner::drop()` (or equivalent teardown path)

## 3. Runtime Hostcall Dispatcher

- [x] 3.1 Add `UdpBind` match arm in `dispatch_hostcall` in `crates/core/runtime/src/hostcall.rs` with `Network` capability check for `ResourceClass::UdpSocket`
- [x] 3.2 Add `ResourceClass::UdpSocket` handling in process resource teardown in `crates/core/runtime/src/process.rs` to call `kernel.close_udp_socket()`

## 4. Guest UDP Module

- [x] 4.1 Create `crates/core/guest/src/net/udp.rs` module with `UdpSocket` struct containing recv/send channel readers/writers and signals (analogous to `TcpStream`)
- [x] 4.2 Implement `UdpSocket::bind(address: impl Into<String>) -> Result<Self>` that calls the `UdpBind` hostcall and attaches the shared region channels
- [x] 4.3 Implement `UdpSocket::attach_shared(shared_id: u64) -> Result<Self>` for attaching to an existing UDP shared region
- [x] 4.4 Implement `UdpSocket::try_recv_from(&mut self, buf: &mut [u8]) -> Result<Option<(usize, SocketAddr)>>` for non-blocking receive of a single datagram
- [x] 4.5 Implement `UdpSocket::try_send_to(&self, buf: &[u8], addr: SocketAddr) -> Result<()>` for non-blocking send of a single datagram
- [x] 4.6 Implement async `UdpSocket::recv_from(&mut self, buf: &mut [u8]) -> Result<(usize, SocketAddr)>` using signal-based waiting
- [x] 4.7 Implement async `UdpSocket::send_to(&self, buf: &[u8], addr: SocketAddr) -> Result<usize>` using signal-based waiting
- [x] 4.8 Implement `UdpSocket::local_addr(&self) -> io::Result<SocketAddr>` returning the bound address
- [x] 4.9 Add `pub mod udp;` to `crates/core/guest/src/net/mod.rs` and export `UdpSocket` from `crates/core/guest/src/lib.rs`

## 5. Tests

- [x] 5.1 Add unit tests for `UdpSocket::attach_shared` with invalid regions (mirroring TCP test patterns)
- [x] 5.2 Add integration test that binds a UDP socket, sends a datagram to itself, and receives it (loopback test)
- [x] 5.3 Verify process teardown correctly cleans up UDP socket resources
