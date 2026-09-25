## Context

Selium runs guest WASM processes on a host runtime, communicating through shared memory (ring buffers, channels, pub/sub, live tables) via the `selium-io` library. All existing I/O patterns are unidirectional and multi-writer. There is no request/reply mechanism, and no tenant isolation on shared memory regions — any guest with a `shared_id` can read, write, or tamper with data belonging to other tenants.

The discovery service (`selium-discovery`) needs an RPC mechanism to serve URI resolution requests, and future services (scheduler, external API) will need it too. Currently the discovery guest is scaffolded with in-memory logic but no I/O wiring.

The host-guest boundary uses `selium-abi` hostcalls for synchronous operations (shared memory allocation, signal creation, network operations) and shared memory ring buffers for high-throughput data. Capability enforcement happens at every hostcall via `CapabilityGrant` + `ResourceSelector` intersection semantics.

## Goals / Non-Goals

**Goals:**
- Secure bidirectional RPC between guests with per-connection memory isolation
- Host-enforced capability gating for RPC connection establishment
- Typed request/reply with compile-time type safety
- Zero-copy data path post-handshake (requests and replies over shared memory)
- Extensible connection acceptance via the `Accept` trait
- Dependency injection of system resources (discovery) into guest entrypoints
- First consumer: discovery service exchange

**Non-Goals:**
- Network-level RPC between hosts (future: QUIC/mTLS bridge)
- Cryptographic message authentication (isolation is architectural, not cryptographic)
- Streaming RPC (single request, single reply per call; streaming is a future extension)
- Modifying existing pub/sub, channel, or live table semantics (beyond the FrameHeader format change)

## Decisions

### D1: Per-connection shared memory isolation

**Decision**: Each client-server RPC pair gets its own `SharedRegion`. No two tenants share a buffer. Connection isolation is enforced by the host only giving `shared_id` to authorised parties.

**Alternatives considered**:
- Single multi-tenant buffer with append-only permissions: rejected because wasmtiny's MMU only understands page-level access, not append-only semantics
- Cryptographic MACs per frame: rejected because it adds per-frame overhead and key management complexity
- Host-mediated per-request hostcalls: rejected because every message would cross the WASM boundary, negating the shared memory performance advantage

### D2: Two sub-memories per SharedRegion (request ring + reply ring)

**Decision**: A single `SharedRegion` allocation contains two sub-memories — a request ring (client writes, server reads) and a reply ring (server writes, client reads). Layout is positional: memory 0 is request, memory 1 is reply.

**Rationale**: Avoids backpressure coupling between request and reply traffic. If the server writes many replies, it doesn't block the client's request writes (which would happen in a single bidirectional ring with strong readers). One allocation means one `shared_id` to exchange during handshake. The `SharedRegionBuilder` constructs the layout; `seal()` makes it immutable.

**Alternatives considered**:
- Single bidirectional ring: rejected due to backpressure coupling and direction ambiguity
- Two separate `SharedRegion` allocations: rejected because it requires two `shared_id`s to exchange during handshake

### D3: Host-mediated connection handshake

**Decision**: Connection establishment goes through a hostcall (`HostQueueSend`), not through shared memory directly. The client sends the session's `shared_id` to the host, which enforces capability checks before enqueuing it to the server's listener. The server receives connections via `ResourceListener::accept()`.

**Rationale**: The host is the trust boundary. It validates that the client is authorised to reach the server before any shared memory is exchanged. No new wasmtiny memory permission types needed.

### D4: Typed Accept trait

**Decision**: Connection acceptance uses a generic `Accept` trait with `type Item`. `RpcAccept<Req, Rep>` implements `Accept` with `Item = RpcConnection<Req, Rep>`. This allows future resource types (e.g. `ProcessAccept`) to use the same `ResourceListener` infrastructure.

```rust
pub trait Accept {
    type Item;
    fn accept(connection: IncomingConnection) -> Result<Self::Item, AcceptError>;
}
```

### D5: FrameHeader changes: 8→12 bytes, writer_id→tag

**Decision**: `FrameHeader` expands from 8 bytes to 12 bytes. `writer_id: u16` becomes `tag: u32`. `flags: u16` becomes `flags: u8` with 3 bytes of reserved padding.

Layout:
```
[len: u32][tag: u32][flags: u8][_reserved: [u8; 3]]
```

`tag` is semantically overloaded: `writer_id` in pub/sub contexts, `correlation_id` in RPC contexts. This is safe because the two code paths never share a ring buffer.

**Rationale**: `u32` correlation IDs allow ~4 billion in-flight requests per connection with wrapping. `u32` writer IDs expand the previous `u16` limit of 65536. The 4-byte overhead per frame is negligible for typical payloads. This is a breaking wire format change but we are pre-release with zero users.

### D6: Connection close via writer_count

**Decision**: When a client or server drops their `RpcClient`/`RpcConnection`, the ring buffer's existing `writer_count` mechanism signals the other side. No new `FLAG_CLOSED` is needed.

**Rationale**: The `ChannelRegion` already tracks `writer_count` with atomic increment/decrement. When it reaches zero, the peer detects `Error::ConnectionClosed` on next read. This avoids adding a new frame type and reuses existing infrastructure.

### D7: Context injection for discovery bootstrap

**Decision**: A `Context` struct is injected into guest `#[entrypoint]` functions, providing a `Discovery` handle (an `RpcClient<DiscoveryRequest, DiscoveryResponse>`) pre-connected to the discovery service.

```rust
pub struct Context {
    discovery: RpcClient<DiscoveryRequest, DiscoveryResponse>,
}

impl Context {
    pub fn from_raw(discovery_handle: u64) -> Result<Self>;
    pub fn discovery(&self) -> &RpcClient<DiscoveryRequest, DiscoveryResponse>;
}
```

The runtime constructs `Context` during guest bootstrap, allocating the session region, connecting to discovery via `ResourceSender`, and providing the fully-formed `RpcClient` to the guest.

**Rationale**: Solves the discovery bootstrap problem without requiring guests to "find" the discovery service. Dependency injection eliminates circular dependencies. The `#[entrypoint]` macro is updated to accept and decode `Context`.

### D8: SharedRegionBuilder for multi-memory regions

**Decision**: A builder pattern constructs `SharedRegion` with multiple sub-memories:

```rust
let region = SharedRegionBuilder::new(SESSION_CAPACITY)
    .add_memory(REQ_BUF)
    .add_memory(REP_BUF)
    .seal()?;
```

Sub-memories are stored contiguously with 8-byte alignment padding. The region header records `memory_count` and `(offset, len)` pairs. After `seal()`, no further modifications are permitted.

### D9: Async throughout, no blocking

**Decision**: `ResourceSender::send`, `RpcClient::request`, `RpcConnection::recv`, and `RpcRequest::reply` all return `Future`s. Timeouts use existing `tokio::time::timeout` rather than custom mechanisms.

## Risks / Trade-offs

[FrameHeader breaking change across all consumers] → All existing `selium-io` code that reads/writes frames must be updated. Acceptable because we have zero users and the change is mechanical (constant update + field rename).

[Per-connection memory allocation] → Each RPC session allocates a `SharedRegion`. Under many concurrent connections, this consumes host shared memory. Mitigation: connection pooling or region reuse aren't in scope yet, but the `SharedRegionBuilder` API doesn't prevent future additions like `SharedRegion::reset()`.

[Positional sub-memory layout is fragile] → If a client constructs a region with memories in the wrong order, only their own connection breaks. The server validates `memory_count == 2` during `Accept`. Future enhancement could add type tags to sub-memory layout.

[Discovery is the only initial consumer] → The RPC API will be shaped heavily by discovery's needs (single request, single reply). Streaming or multiplexed patterns may need API extensions later. The `Accept` trait provides a natural extension point.

[HostQueue send/recv adds new hostcall variants] → Two new ABI entries increase the attack surface for the runtime. Mitigation: these hostcalls are straightforward wrappers around an internal queue with capability checks.

## Open Questions

- Should `ResourceListener` support listening on multiple service URIs simultaneously, or is one listener per URI sufficient for now?
- What is the maximum practical number of concurrent RPC sessions per guest, and does the `SharedRegion` allocation strategy need limits?
- Should `RpcClient::request` support pipelining (multiple in-flight requests on one connection), or is sequential request-await sufficient for discovery?