## Context

Selium guests communicate with the host and each other through two mechanisms: hostcalls (synchronous/async ABI calls) and shared memory (ring buffers with signal-based notification). The current network layer (`NetworkListener`, `NetworkSession`, `NetworkStream`, `RequestExchange`) uses bespoke hostcalls for every operation including per-read/per-write I/O. This makes efficient `AsyncRead`/`AsyncWrite` impossible and blocks integration with tokio-based frameworks like axum.

Meanwhile, the `ResourceListener`/`ResourceSender`/`Accept` infrastructure and the `SharedRegion`/`RingBuf`/`Signal`/`Channel` stack already provide a proven pattern: zero-hostcall I/O through shared-memory ring buffers with signal notification. The `RpcClient::connect` and `RpcConnection::for_server` code (recently refactored) demonstrates the full lifecycle: client creates a `SharedRegion` with two ring buffers and signals, sends the `shared_id` via `ResourceSender`, and the server attaches via `ResourceListener`. `TcpListener`/`TcpStream` should follow the same shape — the only difference is that the **kernel** creates the region (instead of the guest) because it must proxy between real OS sockets and shared memory.

## Goals / Non-Goals

**Goals:**
- Replace bespoke network hostcalls with shared-memory-based I/O, dogfooding the existing `ResourceListener`/`Accept`/`SharedRegion`/`RingBuf`/`Signal` stack
- Enable zero-hostcall `AsyncRead`/`AsyncWrite` on `TcpStream`
- Enable axum integration via `impl axum::serve::Listener for TcpListener`
- Reduce the ABI surface by removing 10 network-specific hostcall variants
- Unify inbound and outbound TCP connections under the same `TcpStream` type

**Non-Goals:**
- UDP support (future change)
- TLS termination (can be layered on top of `TcpStream` in-guest or at the kernel proxy level later)
- Changing the RPC or pub/sub I/O subsystems (they stay as-is)
- Supporting multiple guests sharing a single TCP listener (each listener is bound by one guest)
- Kernel-side load balancing or HTTP routing (the kernel is a dumb byte proxy)

## Decisions

### D1: The kernel creates shared regions for TCP connections

**Decision**: For both inbound and outbound TCP, the kernel creates the `SharedRegion` containing two ring buffers (inbound and outbound) plus two `Signal`s. The guest never creates the region — it only attaches.

**Rationale**: The kernel must proxy bytes between real OS sockets and shared memory. It needs immediate access to the ring buffers to start proxying as soon as the connection is established. If the guest created the region, the kernel would need to wait for a hostcall providing the `shared_id` before proxying could begin, adding latency and complexity.

**Alternative considered**: Guest creates the region and sends the `shared_id` to the kernel (like `RpcClient::connect`). Rejected because: (a) the kernel needs to proxy immediately, (b) outbound connections need the proxy running before the guest can even reference the region, (c) adds an unnecessary round-trip.

### D2: Inbound connections use HostQueue (ResourceListener)

**Decision**: `TcpListener::bind` returns a `HostQueue` descriptor. The kernel enqueues `{ client_process_id: 0, value: shared_id }` for each accepted connection. The guest calls `ResourceListener::accept::<TcpAccept>()` and `TcpAccept::accept(IncomingConnection)` produces a `TcpStream`.

**Rationale**: Reuses the proven `ResourceListener`/`Accept` pattern already used for RPC. The `TcpAccept` impl calls `TcpStream::attach_shared(connection.shared_id)` which maps ring buffers from the shared region. The `client_process_id` field is set to 0 for external TCP connections (they come from outside the guest system).

### D3: Outbound connections return SharedRegionDescriptor

**Decision**: `TcpStream::connect(addr)` calls `hostcall_async(TcpConnect { address })`. The kernel opens a real TCP connection, creates the shared region, starts the proxy, and returns `HostcallOutput::SharedRegion(SharedRegionDescriptor)`. The guest internally calls `TcpStream::attach_shared(descriptor.shared_id)`.

**Rationale**: Uses the existing `HostcallOutput::SharedRegion` variant — no new output type needed. The guest then performs the same `attach_tcp_channels` procedure as inbound connections.

### D4: Shared-memory layout is the same two-channel format as RPC

**Decision**: TCP stream shared regions contain exactly 2 sub-memories (inbound ring, outbound ring), using the same `SharedRegionBuilder` / multi-memory layout that `RpcClient::connect` uses. Channel 0 is inbound (kernel writes, guest reads). Channel 1 is outbound (guest writes, kernel reads).

**Rationale**: Reuses the proven `attach_rpc_channels` / `SharedRegion` / `ChannelRegion` / `RingBuf` / `Signal` stack with zero modification. A shared helper function (`attach_stream_channels` or generalised from `attach_rpc_channels`) handles attachment on the guest side.

### D5: TcpStream implements AsyncRead + AsyncWrite via ring buffers and signals

**Decision**: `TcpStream` holds a `StrongReader` on the inbound channel and a `StrongWriter` on the outbound channel. `AsyncRead::poll_read` reads from the inbound ring buffer; on `ChannelEmpty` it signals and returns `Poll::Pending`. `AsyncWrite::poll_write` writes to the outbound ring buffer; the `StrongWriter` auto-notifies the associated signal, waking the kernel proxy.

**Rationale**: This is the same read/wait pattern used by `RpcConnection::recv` and `Subscriber::read` + `wait`. No new I/O mechanism needed. The guest-behavioural pattern is proven.

### D6: Kernel network runtime uses tokio TcpListener + proxy tasks

**Decision**: A kernel-side `network` module spawns a `tokio::net::TcpListener` per `TcpBind` hostcall. On accept, it creates the shared region + ring buffers + signals, starts a bidirectional proxy `tokio::task`, and enqueues the `shared_id` into the `HostQueue`. The proxy task does `tokio::select!` between real-socket-reads and signal-waits (guest wrote to outbound ring).

**Rationale**: The kernel already uses tokio for async operations (`Notify` for signals, `host_queue_recv`). Proxy tasks are lightweight tokio tasks — one per active TCP connection. The kernel's `SignalState` already has `tokio::sync::Notify` which can be `notified().await`'d by proxy tasks.

### D7: Guest→kernel notification uses Signal (no hostcall)

**Decision**: When the guest writes to the outbound ring, the `StrongWriter` auto-notifies the outbound signal. The kernel proxy task is `select!`'d on this signal's `Notify`. No hostcall is needed per write — the kernel wakes on the same signal the guest sends.

**Rationale**: The `StrongWriter` already stores an optional signal and calls `signal.notify()` after each write. The kernel's `SignalState::notify()` calls `notify.notify_waiters()`. The proxy task waits on `notify.notified()`. This is zero-hostcall I/O in both directions.

### D8: Connection close detection uses writer count

**Decision**: When a `TcpStream` drops, the guest decrements the outbound ring's writer count. The kernel proxy detects `writer_count == 0` and shuts down the write side of the real TCP socket. When the real socket reaches EOF, the kernel proxy decrements its inbound ring's writer count. The guest's `AsyncRead` detects `writer_count == 0` and returns `Ok(0)` (EOF).

**Rationale**: This is the same close-detection pattern used by `RpcConnection::recv` (checks `read_writer_count() == 0`) and `Subscriber` (same pattern). Proven and consistent.

## Risks / Trade-offs

- **[Backpressure]** If the guest reads slowly, the inbound ring buffer fills up. The kernel proxy must stop reading from the real socket until the guest catches up. Mitigation: The `StrongWriter` backpressure mechanism (won't overwrite data that strong readers haven't consumed) already exists. The kernel proxy simply waits on the signal when the ring is full. → Risk mitigated by existing ring buffer semantics.

- **[ABI break]** Removing 10 network hostcall variants is a breaking change to `HostcallRequest`. Any code consuming the old variants will need updating. Mitigation: This is a intentional simplification. The entire `network` module is being replaced.

- **[Ring buffer sizing]** Fixed ring buffer capacity must be chosen for TCP streams. Too small = poor throughput; too large = wasted memory per connection. Mitigation: Make capacity configurable via `TcpListener::bind` and `TcpStream::connect` parameters, with sensible defaults (e.g., 64KB per channel).

- **[Kernel complexity]** The network runtime adds substantial kernel-side code: TCP listener management, proxy tasks per connection, kernel-side ring buffer operations, signal wiring. Mitigation: The kernel already has shared memory primitives; ring buffer logic mirrors the guest-side `RingBuf`. Proxy tasks are straightforward `tokio::select!` loops.

- **[Latency on connect]** For outbound connections, the guest must wait for `hostcall_async(TcpConnect)` to complete (TCP handshake + region setup) before receiving the `SharedRegionDescriptor`. This is unavoidable but acceptable — it's a one-time cost per connection.

- **[axum Listener trait compatibility]** The `Listener` trait may require synchronous `accept()` in some versions. The underlying `ResourceListener::accept` is async. Mitigation: Target axum versions where `Listener::accept` returns a `Future`. If needed, use `tokio::task::block_in_place`.

## Migration Plan

1. Add new `HostcallRequest` variants (`TcpBind`, `TcpConnect`) and kernel handlers alongside existing network hostcalls
2. Implement `TcpListener`, `TcpStream`, `TcpAccept` in `selium-guest`
3. Implement kernel network runtime with proxy tasks
4. Add `AsyncRead`/`AsyncWrite` impls for `TcpStream`
5. Add `axum` feature flag with `Listener` impl
6. Update guest applications to use new types
7. Remove old network hostcalls, descriptors, and types from `selium-abi`, `selium-guest`, and `selium-kernel`

**Rollback**: The old and new code can coexist during migration. Remove old code only after all guests are updated.

## Open Questions

- Should `TcpConnect` return a `SharedRegionDescriptor` (reusing existing variant) or a new `TcpStreamDescriptor` type? The former is simpler; the latter provides more type safety. Leaning toward `SharedRegionDescriptor` for minimal ABI surface.
- What default ring buffer capacity should the kernel use for TCP streams? 64KB per channel (128KB total per connection) is the initial proposal but needs benchmarking.
- Should the kernel support TCP keepalive, Nagle algorithm toggling, or other socket options? Initially no — raw byte streams only. Socket options can be added as hostcalls later if needed.
- How to handle the `client_process_id` field in `HostQueueRecv` output for external connections? Setting it to 0 is a temporary convention. A sentinel value or tagged enum might be cleaner.