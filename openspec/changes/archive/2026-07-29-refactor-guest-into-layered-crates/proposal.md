## Why

`selium-guest` started as a WASM SDK for guest modules talking to the host ABI, but has accumulated three roles — WASM guest library, native I/O pattern library (used by `selium-runtime`), and the seed for an external client library over QUIC — all gated through `cfg(target_arch = "wasm32")` branching. This conflates transport media (shared-memory vs. QUIC) with pattern semantics (pubsub, RPC), prevents the runtime from linking cleanly without pulling in guest-side hostcall stubs, and leaves the external client use case unaddressable without extending the fragile `cfg` design further. The platform needs the I/O patterns reusable everywhere, shared memory co-located only, and QUIC bridging done by guests (not the runtime) to preserve the sandbox security model.

## What Changes

- **BREAKING**: Split `selium-guest` into five layered crates: `selium-memory`, `selium-encoding`, `selium-wire`, `selium-shm`, and a slimmed `selium-guest` SDK
- Introduce a `RegionProvider` trait in `selium-memory` to abstract shared-memory allocation behind an injectable backend, replacing `cfg(wasm32)` branching
- Introduce a `MessageTransport` trait in `selium-wire` so pubsub, RPC, and live tables are written once against an abstract duplex channel — not hard-wired to ring buffers
- Move the frame/ring/channel primitives into `selium-shm`, which implements `MessageTransport` for co-located peers only
- Extract FlatBuffers encoding, codec, and schema types into `selium-encoding` for use by all consumers without guest baggage
- Create `selium-quic` (guest-side QUIC `MessageTransport` impl, lifted from `net/quinn.rs`) shared by the external client lib and bridge guests
- Introduce the **guest-bridge** pattern: per-user WASM guests that accept QUIC connections and transparently relay `selium-wire` frames into shared-memory rings, scoped to a single external user
- Runtime drops its direct `selium-guest` dependency; depends only on `selium-wire` + `selium-shm` + `selium-memory` + `selium-encoding`
- External clients depend only on `selium-wire` + `selium-quic` — never link any hostcall, shared-memory, or region code

## Capabilities

### New Capabilities

- `transport-abstraction`: A `MessageTransport` trait providing duplex framed I/O (`AsyncRead` + `AsyncWrite`), readiness polling, peer-closed detection, and lag notification, enabling pubsub/RPC patterns to be written once against the trait and instantiated over different media (shared-memory rings, QUIC streams).
- `region-provider`: A `RegionProvider` trait (`allocate`, `attach`, `free`) that abstracts shared-memory region lifecycle behind an injectable backend. Concrete implementations: hostcall-backed for WASM guests, region-table-backed for the runtime, heap-backed for tests and in-process use.
- `guest-bridge`: A per-user WASM guest that terminates a QUIC connection from an external client, transparently proxies `selium-wire` frames into shared-memory rings, and enforces the user's capability grants. The bridge is composed from the same `selium-wire` patterns as any other guest.

### Modified Capabilities

- `framed-io`: Framed read/write abstractions become transport-generic. `FramedRead`/`FramedWrite` operate over any `MessageTransport` rather than being ring-specific. Side-channel coordination (`generation()`, `load_writer_count()`) moves behind the transport trait.
- `selium-rpc`: `RpcClient`/`RpcConnection` become generic over `MessageTransport` instead of depending on `crate::io::channels` directly. Connection establishment (`connect`/`accept`) is parameterized by a `Rendezvous` trait rather than hard-wired to `ResourceSender`.
- `selium-guest`: Shrinks to a WASM SDK only. It installs the hostcall-backed `RegionProvider` and mailbox reactor, provides `net/*` (including QUIC datagram transport for guests), resources, storage, process management, logging subscriber, and re-exports the lower crates to preserve the existing public API surface.
- `guest-shared-memory`: `SharedRegion::allocate`/`attach`/`free` become methods on a `RegionProvider` trait implementation rather than `cfg`-gated free functions. The memory module itself (`RegionMapping` + atomics) is unaffected.
- `quinn-transport`: QUIC stream framing moves from `selium-guest::net::quinn` into `selium-quic` as a `MessageTransport` impl. The guest retains the UDP datagram hostcall plumbing needed to *feed* quinn, but the stream framing is shared with the external client.
- `resource-handshake`: Connection rendezvous (passing `shared_id` from client to server) becomes a `Rendezvous` trait used by `selium-wire` patterns. The existing `ResourceSender`/`ResourceListener` hostcall-backed impl becomes one concrete implementation used by guests; the runtime supplies its own.

## Impact

- **Crates created**: `crates/core/memory`, `crates/core/encoding`, `crates/core/wire`, `crates/core/shm` (all under `crates/core/`); `crates/guests/bridge` (the reference bridge guest)
- **Crates modified**: `selium-guest` (slimmed + re-exports), `selium-runtime` (drops `selium-guest` dep, adds `selium-wire` + `selium-shm`), all existing guest crates in `crates/guests/` (transparent re-export keeps compile unchanged)
- **Crate created later**: `selium-quic` (shared by external client lib and bridge guest; location TBD)
- **Public API**: Existing `selium-guest` public surface preserved via re-exports; no guest code changes required
- **Dependencies removed**: `selium-runtime` no longer links `selium-guest` (eliminates the guest-side `extern "C"` hostcall imports leaking into the host)
- **Breaking for runtime internals**: Runtime code that imported `selium_guest::io::rpc` or `selium_guest::FlatMsg` must change to `selium_wire::rpc` and `selium_encoding::FlatMsg`
