## Context

The existing TCP networking support follows a connection-oriented model: `TcpBind` returns a `HostQueue` that the guest polls for incoming connections, and each accepted connection gets a dedicated pair of shared-memory ring buffers (one inbound, one outbound). UDP is fundamentally different — it is connectionless and datagram-oriented. A single bound UDP socket handles all peers on one send/receive channel pair.

The kernel already has the infrastructure for shared-memory ring buffers, signals, and OS-thread-based proxy loops. The UDP design reuses these same primitives with a different channel topology.

## Goals / Non-Goals

**Goals:**
- Add a `UdpBind` hostcall that binds a real OS UDP socket and exposes it to a guest via shared-memory channels
- Provide a `selium_guest::net::udp::UdpSocket` type for sending and receiving datagrams
- Implement kernel-side UDP proxy threads that bridge between the OS socket and guest channels
- Support full lifecycle: bind, recv, send, close
- Follow the same architectural patterns as the existing TCP module

**Non-Goals:**
- Quinn integration (deferred to a separate change)
- High-performance optimizations (GSO, GRO, segment offload) — single-datagram per channel frame is sufficient initially
- Connected UDP sockets (`connect(2)`) — addressing is always explicit per datagram
- ICMP or error reporting beyond `WouldBlock` / basic I/O errors

## Decisions

### 1. Single `UdpBind` hostcall (no separate `UdpConnect`)

TCP has two hostcalls (`TcpBind` for listening, `TcpConnect` for outbound connections) because TCP distinguishes server vs client roles. UDP is connectionless — a single bound socket sends to and receives from any peer. A single `UdpBind` hostcall covers both cases.

**Alternatives considered:**
- `UdpCreate` + separate `UdpBind` — unnecessary indirection; `UdpBind` is the natural primitive since a UDP socket must be bound before use
- `UdpConnect(address)` for setting a default destination — not needed for Quinn (which manages its own addressing) and complicates the channel protocol. Can be added later if required.

### 2. Shared-memory channel pair per socket (not per peer)

Unlike TCP where each connection has dedicated channels, a single UDP socket gets one receive channel and one send channel. All peers share the same channels. This matches Quinn's `AsyncUdpSocket` model where `poll_recv` returns datagrams from any source and `poll_send` sends to an explicit destination.

**Channel layout (same header convention as TCP):**

```
Shared Region Header (256 bytes)
  ├── magic: u64 = SHARED_REGION_MAGIC
  ├── memory_count: u32 = 2
  ├── memory[0]: recv_channel offset + length
  └── memory[1]: send_channel offset + length

Recv Channel (kernel → guest):
  - Ring buffer of datagram frames
  - Each frame: [header_12bytes][source_addr][ecn_byte][payload]

Send Channel (guest → kernel):
  - Ring buffer of datagram frames
  - Each frame: [header_12bytes][dest_addr][payload]
```

### 3. Recv frame format includes source address metadata

Each received datagram frame carries:
- **Header** (12 bytes, same as TCP): `{ len: u32, tag: u32, flags: u8, _reserved: [u8; 3] }`
- **Source address**: encoded as `SocketAddr` (2-byte length prefix + addr bytes)
- **ECN byte**: 1 byte for explicit congestion notification
- **Payload**: the raw UDP datagram bytes

For outbound frames, the format is:
- **Header** (12 bytes)
- **Destination address**: encoded as `SocketAddr`
- **Payload**: the raw UDP datagram bytes

### 4. Kernel proxy uses two threads (same pattern as TCP)

```
UdpBind(address)
  │
  ├── std::net::UdpSocket::bind(address)
  ├── Allocate shared region with 2 ring buffers + 2 signals
  ├── Spawn proxy_recv_thread (OS thread):
  │     Loop:
  │       recvfrom() → write frame to recv_channel → signal guest
  │
  ├── Spawn proxy_send_thread (OS thread):
  │     Loop:
  │       Wait on send_channel signal → read frame → sendto()
  │
  └── Return SharedRegionDescriptor to guest
```

### 5. Guest-side `UdpSocket` implements `send_to` / `recv_from` methods

Unlike TCP's `AsyncRead`/`AsyncWrite` (byte-stream oriented), UDP exposes datagram-oriented methods:

```rust
impl UdpSocket {
    pub async fn bind(address: impl Into<String>) -> Result<Self>;
    pub async fn send_to(&self, buf: &[u8], addr: SocketAddr) -> Result<usize>;
    pub async fn recv_from(&self, buf: &mut [u8]) -> Result<(usize, SocketAddr)>;
    pub fn local_addr(&self) -> io::Result<SocketAddr>;
    pub fn try_send_to(&self, buf: &[u8], addr: SocketAddr) -> Result<()>;
    pub fn try_recv_from(&self, buf: &mut [u8]) -> Result<Option<(usize, SocketAddr)>>;
}
```

These are the methods Quinn's `AsyncUdpSocket` needs internally (addressed in a future change).

### 6. Single `ResourceClass::UdpSocket` (not listener + stream)

Unlike TCP (which has `TcpListener` and `TcpStream`), UDP needs only one resource class since a bound socket is both a sender and receiver.

### 7. Resource management follows TCP pattern

- `KernelInner` gets `udp_sockets: Mutex<HashMap<SharedResourceId, UdpSocketState>>`
- Process teardown calls `close_udp_socket` which sets `running = false` and notifies both signals
- `UdpSocketState` stores:
  ```rust
  struct UdpSocketState {
      running: Arc<AtomicBool>,
      recv_signal: Arc<SignalState>,
      send_signal: Arc<SignalState>,
  }
  ```

### 8. No new `HostcallOutput` variant — reuse `SharedRegion`

The existing `HostcallOutput::SharedRegion(SharedRegionDescriptor)` is sufficient. The shared region contains the two ring buffers and the signal IDs are embedded in the region headers (same mechanism as TCP's `attach_tcp_channels`).

## Risks / Trade-offs

| Risk | Mitigation |
|------|------------|
| Single receive channel means head-of-line blocking if a slow consumer reads large datagrams | Same model as TCP; Quinn batches reads and the proxy can drop datagrams if the channel is full (UDP is lossy by design) |
| GSO/GRO not supported initially — lower throughput for large QUIC packets | Single-segment mode is sufficient for correctness; GSO/GRO can be added later via frame flags when Quinn integration lands |
| Proxy threads are OS-thread-per-socket (same as TCP) which doesn't scale to thousands of sockets | Acceptable for arch3 prototype; a future io_uring or event-based proxy could replace the thread-per-socket model |
| No ECN or dst_ip metadata propagation | Recv frame format reserves the ECN byte; initial impl may leave it zeroed. dst_ip propagation can be added when the kernel-side plumbing exists |
