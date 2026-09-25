## Context

`selium-guest` (at `crates/core/guest/`) is the current Selium guest SDK. It started as a WASM library but now serves three consumers via `#[cfg(target_arch = "wasm32")]` branching:

1. **WASM guests** — the original target, using hostcalls for resource allocation and a mailbox-based async reactor
2. **selium-runtime** — uses `io::rpc::RpcClient`, `FlatMsg`, and `log::LogRecord` from the guest crate, falling into the `not(wasm32)` arm with stub hostcalls
3. **External client** (future) — needs QUIC-based I/O patterns to talk to guests remotely

The crate mixes transport-agnostic concerns (frame format, encoding, pubsub/RPC pattern semantics) with transport-specific concerns (shared-memory ring buffers, hostcall stubs, WASM atomics) and environment-specific concerns (hostcall backend vs. native registry). The runtime accidentally uses test scaffolding (`NATIVE_REGION_REGISTRY`) as production plumbing.

The platform thesis requires untrusted I/O parsing in the sandbox (guest-bridge pattern), not in the host (runtime-bridge). The refactor enables this by separating protocol from transport and making the bridge a normal deployable guest.

## Goals / Non-Goals

**Goals:**
- Split `selium-guest` into layered crates where each consumer pulls only what it needs
- Replace `#[cfg(target_arch = "wasm32")]` branching in I/O and memory code with trait-based provider injection
- Make pubsub, RPC, and live table patterns generic over a `MessageTransport` trait so they work over shared-memory and QUIC identically
- Enable the guest-bridge pattern: per-user WASM guests that transparently relay `selium-wire` frames between QUIC streams and shared-memory rings
- Preserve the existing `selium-guest` public API surface via re-exports so no guest code breaks

**Non-Goals:**
- Changing the ring buffer coordination layout or frame format
- Adding a QUIC stack to the runtime
- Supporting RDMA or other remote memory models
- Implementing the external client library itself (that's a follow-on change)
- Removing the `io`, `logging`, or `quinn` feature flags from `selium-guest`

## Decisions

### Decision 1: Five-crate split

```
selium-abi          (unchanged)
  ▲
selium-memory    ── RegionMapping (pure ptr+atomics) + RegionProvider trait + HeapRegionProvider
  ▲
selium-encoding  ── FlatMsg, HasSchema, fbs/, codec, LogRecord types
  ▲
selium-wire      ── FrameHeader/FrameCodec, MessageTransport trait, FramedRead<M>/FramedWrite<M>,
                     Publisher<T,M>, Subscriber<T,M>, RpcClient<Req,Rep,M>, RpcConnection<Req,Rep,M>,
                     LiveTable<K,V,M>, Rendezvous trait
  ▲
selium-shm       ── ChannelRegion, RingBuf, Reader/Writer/WeakReader/WeakWriter, ShmTransport (impl MessageTransport)
  ▲
selium-guest     ── HostcallRegionProvider, mailbox reactor, platform/hostcall bridge,
                     ResourceSender/ResourceListener, net/*, storage, process, context, log subscriber,
                     re-exports all of the above
```

`selium-quic` (separate crate) ── `QuicTransport` (impl `MessageTransport`), shared by external client + bridge guests

**Rationale:** Each crate has one dependency direction (upward in the diagram). No crate depends on a higher crate. `selium-wire` is the "pattern library" — it depends on `selium-encoding` for payload serialization and on `tokio` I/O traits, but NOT on any specific transport. `selium-shm` and `selium-quic` are peer transport implementations.

**Alternative considered:** Put transport trait in `selium-shm` and have `selium-quic` depend on it. Rejected because it makes QUIC depend on shared-memory concepts and muddies the dependency direction.

### Decision 2: Global provider injection over generic parameters

`RegionProvider` is installed globally via `OnceLock<Box<dyn RegionProvider>>` rather than threaded as a generic parameter through every type. Rationale:

- `Publisher::create(capacity)` stays ergonomic — no `<P: RegionProvider>` on every struct
- The provider is genuinely process-global (you have one allocation backend)
- Matches the existing pattern of the mailbox reactor being installed once
- Types that need the provider call `region_provider().allocate(...)` internally

Same pattern for the async `Yielder` (used by RPC polling loops): installed once, called as `yield_now().await`.

**Alternative considered:** Generic `RegionProvider` on `RingBuf`, `Channel`, etc. Rejected because it would infect every type parameter up to `Publisher<T, W, P>` — a bad ergonomic tax for something that's singleton in practice.

### Decision 3: MessageTransport as a trait composing AsyncRead + AsyncWrite

```rust
pub trait MessageTransport: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin {
    type Error: std::error::Error + From<io::Error>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Result<bool, Self::Error>;
    fn poll_peer_closed(&mut self, cx: &mut Context<'_>) -> Result<bool, Self::Error>;
    fn generation(&self) -> Result<u64, Self::Error>;
}
```

`AsyncRead` + `AsyncWrite` provide the byte-stream I/O that `FrameHeader` codec already uses. The three additional methods abstract the side channel currently hard-wired to ring buffer fields:

| Transport | `poll_ready` | `poll_peer_closed` | `generation` |
|---|---|---|---|
| `ShmTransport` | reader.poll_ready() → frame available? | writer_count == 0 | ring generation counter |
| `QuicTransport` | stream has readable bytes? | stream FIN/RESET | 0 (not supported) |

**Rationale:** Avoids rewriting the byte-level framing. The `FrameHeader` codec already works over `AsyncRead`/`AsyncWrite`. We're only lifting the coordination methods that the patterns bypass the byte stream to call.

**Alternative considered:** A `Stream` + `Sink` based trait. Rejected because it forces a different I/O model and loses compatibility with the existing `tokio::codec` infrastructure.

### Decision 4: Guest-bridge over runtime-bridge

QUIC termination runs in a WASM guest (the bridge), not in the runtime. The runtime provides only UDP datagram hostcalls. Rationale:

- Untrusted QUIC+TLS+frame parsing stays sandboxed — a parser exploit compromises one bridge, not the host
- The existing capability system enforces which channels the bridge can attach to (the bridge holds the user's `CapabilityGrant`s)
- Metering and billing work naturally (the bridge is a normal guest)
- Protocol evolution (QUIC versions, new transports, custom auth) = guest deployments, not runtime releases
- The bridge is composed from the same `selium-wire` patterns users already write — it's a normal guest, not privileged host code

**Alternative considered:** Runtime-bridge (native QUIC in the host). Rejected because it puts hostile-parsing code in the TCB, requires reimplementing capability checks in native code, and creates a privileged code path that diverges from the guest model.

### Decision 5: Bridge scoped per external user

One bridge guest = one external user (one QUIC connection), multiplexing the user's channels as QUIC streams. An acceptor guest owns the public endpoint and spawns bridge guests on new connections.

**Rationale:**
- Per-channel: wastes QUIC multiplexing, guest explosion
- Many-users-per-bridge: shared fate, capability muddling
- Per-user: clean authority (bridge holds exactly that user's grants), clean blast radius, maps naturally to QUIC connection-per-user

### Decision 6: Rendezvous trait for connection establishment

RPC connection setup (passing `shared_id` from client to server) is abstracted behind a `Rendezvous` trait. `ResourceSender`/`ResourceListener` is one impl (hostcall-backed, for guests). The runtime supplies its own impl. The external client will have a QUIC-based impl.

**Rationale:** This is the last hostcall coupling in patterns. Breaking it lets the runtime do RPC without ever calling a hostcall.

## Risks / Trade-offs

**[Risk] Global provider state may cause test contamination** → Mitigation: `HeapRegionProvider` is the default in non-wasm; tests that need isolation can create fresh providers and install/restore around tests using a guard pattern (similar to `tokio::runtime::EnterGuard`).

**[Risk] `MessageTransport` trait may grow as new transport-specific needs emerge** → Mitigation: Start minimal with the three side-channel methods that have clear semantics across transports. Add optional methods with default impls if needed.

**[Risk] Per-user bridge guests add connection latency (guest spawn per connection)** → Mitigation: Pre-spawn bridge pools for known users; the scheduler already supports fast process start. Measure before optimizing. The security win justifies the overhead for the common case.

**[Risk] Breaking change for `selium-runtime` imports** → Mitigation: The change is contained to runtime internals. The public guest API is preserved via re-exports. Runtime changes are mechanical (`selium_guest::io::rpc` → `selium_wire::rpc`).

**[Trade-off] QUIC stream per channel vs. multiplexed frames on one stream** → Chosen: one QUIC stream per channel. Streams get native QUIC flow control, backpressure, and cancellation per-channel for free. Multiplexing frames on one stream would require reinventing those in user space.

## Migration Plan

1. **Create `selium-memory`** — move `memory.rs` as-is, extract `RegionProvider` trait, move heap registry to `HeapRegionProvider`, add global installation. `selium-guest` depends on it (re-exports `RegionMapping`, `SharedRegion` for compat).

2. **Create `selium-encoding`** — move `encoding.rs`, `fbs/`, `codec.rs`, `LogRecord`/`LogLevel`/`LogField` types. Fix `#[schema]` macro paths. `selium-guest` depends on it (re-exports `FlatMsg`, `HasSchema`, `LogRecord`).

3. **Create `selium-wire`** — define `MessageTransport` trait, move `frame.rs`/`framed.rs`, create transport-generic `Publisher`/`Subscriber`/`RpcClient`/`RpcConnection`. Depends on `selium-encoding`.

4. **Create `selium-shm`** — move `region.rs`/`ring_buf.rs`/`channels/`/`cursor.rs`, implement `ShmTransport: MessageTransport`, use global `RegionProvider` instead of `SharedRegion::allocate` directly. Depends on `selium-memory` + `selium-wire`.

5. **Slim `selium-guest`** — keep `platform.rs`/`hostcall.rs`/`async_runtime.rs`/`resource.rs`/`storage.rs`/`process.rs`/`net/*`/`time.rs`/`context.rs`/`log.rs` subscriber. Implement `HostcallRegionProvider`, install it + mailbox reactor at guest init. Re-export `selium-wire`, `selium-shm`, `selium-encoding`, `selium-memory` to preserve public API.

6. **Repoint `selium-runtime`** — change imports from `selium_guest::*` to the new crate paths. Install its own `RegionProvider` (runtime region table). Drop the `selium-guest` dependency.

7. **Create reference bridge guest** at `crates/guests/bridge/` — uses `selium-guest` + `selium-quic`, accepts QUIC connections, relays frames. Validates the full stack.

Each step compiles green before the next step starts.

## Open Questions

1. **`selium-quic` location**: Should it live under `crates/core/` (shared by external client and bridge guests) or under a new `crates/transports/` directory? Leaning `crates/core/quic/` for discoverability.

2. **Acceptor guest**: Should the QUIC acceptor be a dedicated system guest (`crates/guests/acceptor/`) or folded into the discovery guest (since discovery already owns the well-known endpoint)? Leaning dedicated acceptor for clean separation.

3. **`LiveTable` transport-generic**: Does `LiveTable` need `MessageTransport` at all, or does it use a subscription internally and only the subscription needs transport? Need to examine the impl before deciding.

4. **Backward compat shim duration**: How long should `selium-guest` retain re-exports of `SharedRegion` (now in `selium-memory`)? Indefinitely (via deprecation) or remove after all guests migrate? Leaning indefinite re-export with no deprecation since it's zero-cost.

5. **`selium-wire` naming**: Alternative name `selium-patterns` considered. `selium-wire` chosen because it contains the wire format (frame header) and protocol definitions. Feedback welcome.
