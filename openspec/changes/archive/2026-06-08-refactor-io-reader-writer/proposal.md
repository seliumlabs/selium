## Why

The I/O type hierarchy has accumulated fragmentation and duplication: `StrongReader`/`WeakReader` are bypassed by every network transport (`TcpStream`, `QuinnUdpSocket`, `UdpSocket`) and every pattern (`RpcClient`, `Context`), all of which work with raw `RingBuf` instead. The `crates/patterns/` crates duplicate code already present in `crates/core/guest/src/io/` — `LiveTable` exists in both places, and `Context` reimplements RPC client logic inline because `selium-rpc` depends on `selium-guest`, creating a circular dependency. The Quinn integration has stubs marked TODO from a prior networking refactor, and `unsafe impl Send/Sync` blocks lack safety documentation.

This change consolidates the I/O types, eliminates duplication, closes the circular dependency, and establishes a clean layering: byte-stream `Reader`/`Writer` → framing-aware `FramedRead`/`FramedWrite` → typed patterns (pub/sub, RPC, tables).

## What Changes

- **BREAKING**: `StrongReader`, `StrongWriter`, `WeakReader`, `WeakWriter` replace their freestanding `read`/`write`/`poll_ready` methods with `tokio::io::AsyncRead` and `tokio::io::AsyncWrite` impls (byte-stream semantics)
- **BREAKING**: `Reader` and `Writer` enums replaced by concrete `Reader` (strong byte-stream) and `Writer` (strong byte-stream) types; `WeakReader`/`WeakWriter` remain as weak variants
- **BREAKING**: `crates/patterns/rpc/` and `crates/patterns/tables/` folded into `crates/core/guest/src/io/`; the `selium-rpc` and `selium-tables` workspace crates removed; their public API re-exported from `selium-guest`
- Add `FramedRead<R>` and `FramedWrite<W>` types that wrap any byte-stream reader/writer to provide frame-level read/write with tag-based correlation (used by pub/sub and RPC)
- Add `Reader::downgrade` / `WeakReader::upgrade` and `Writer::downgrade` / `WeakWriter::upgrade` methods
- Add `Subscriber::upgrade` / `Subscriber::downgrade` and `Publisher::upgrade` / `Publisher::downgrade` to change between strong and weak backing handles
- `Subscriber::check_overwritten` (and `Error::Overwritten`) propagate through `Reader::poll_read` as an `io::Error`
- `Context` uses `RpcClient<DiscoveryRequest, DiscoveryResponse>` directly instead of inline RPC logic (circular dependency resolved by fold)
- `LiveTable` deduplicated: keep the `io/tables.rs` version (with rkyv derives), remove `crates/patterns/tables/`
- `TcpStream` delegates to `Reader`/`Writer` internally instead of raw `RingBuf`
- Implement `QuinnUdpSender::poll_send` and `QuinnUdpSocket::poll_recv` using shared-memory ring buffers
- Add safety comments to `unsafe impl Send/Sync for RegionMappingInner`
- Simplify `RegionProt` ↔ `wasmtiny::RegionProt` conversion

## Capabilities

### New Capabilities
- `framed-io`: `FramedRead<R>` and `FramedWrite<W>` types that wrap any `AsyncRead`/`AsyncWrite` to provide frame-level read/write with `FrameHeader` encoding/decoding and tag-based correlation. Enables pub/sub and RPC patterns to work generically over any byte-stream transport without duplicating framing logic.

### Modified Capabilities
- `selium-guest`: `Reader`/`Writer` types become byte-stream oriented with `AsyncRead`/`AsyncWrite` impls. Strong/weak semantics via upgrade/downgrade. `Subscriber`/`Publisher` use `FramedRead`/`FramedWrite`. `Overwritten` error propagates through `poll_read`. `LiveTable` deduplicated. Safety comments added. RPC and tables modules folded in from removed pattern crates.
- `selium-rpc`: RPC types move from `crates/patterns/rpc/` into `selium-guest::io::rpc`. Crate removed. Spec updated to reflect new home in `selium-guest`.
- `guest-context`: `Context::lookup` uses `RpcClient<DiscoveryRequest, DiscoveryResponse>` from `selium-guest::io::rpc` instead of inline RPC framing logic.
- `guest-networking`: `TcpStream` uses `Reader`/`Writer` internally. `QuinnUdpSender::poll_send` and `QuinnUdpSocket::poll_recv` implemented.

## Impact

- **Crates removed**: `selium-rpc` (`crates/patterns/rpc/`), `selium-tables` (`crates/patterns/tables/`)
- **Crates modified**: `selium-guest` (major API surface change), workspace `Cargo.toml` (members, dependencies), all guest crates that import `selium-rpc` or `selium-tables`
- **Affected guests**: `selium-scheduler`, `selium-discovery`, `selium-cluster`, `selium-supervisor`, `selium-external-api` — import paths change from `selium-rpc`/`selium-tables` to `selium-guest::io::rpc`/`selium-guest::io::tables`
- **No host-side changes**: runtime, kernel, and ABI crates are unaffected
