## 1. Create `selium-memory` crate

- [x] 1.1 Scaffold `crates/core/memory/` with `Cargo.toml` (depends on `selium-abi` only, no hostcalls, no tokio)
- [x] 1.2 Move `memory.rs` (`RegionMapping` + atomics + sub_region) verbatim from `selium-guest` to `selium-memory`
- [x] 1.3 Define `RegionProvider` trait (`allocate`, `attach`, `free`) and `Region` handle type in `selium-memory`
- [x] 1.4 Implement `HeapRegionProvider` (today's `NATIVE_REGION_REGISTRY` logic) in `selium-memory`
- [x] 1.5 Add global provider installation (`set_region_provider` / `region_provider` via `OnceLock`)
- [x] 1.6 Add `selium-memory` to workspace `Cargo.toml` members and `[workspace.dependencies]`
- [x] 1.7 Add `selium-guest` dependency on `selium-memory`; re-export `RegionMapping`, `PAGE_SIZE`, `SHARED_REGION_MAGIC` for backward compat
- [x] 1.8 Verify `selium-guest` compiles and all existing tests pass (re-exports unchanged public API)

## 2. Create `selium-encoding` crate

- [x] 2.1 Scaffold `crates/core/encoding/` with `Cargo.toml` (depends on `flatbuffers`, `selium-abi`, `selium-guest-macros`)
- [x] 2.2 Move `encoding.rs` (`FlatMsg`, `HasSchema`, `FieldEncoder`, `SchemaDescriptor`, wire types + impls) verbatim
- [x] 2.3 Move `codec.rs` verbatim
- [x] 2.4 Move `fbs/` directory verbatim; fix `#[schema(... binding = "...")]` paths from `crate::fbs::...` to `selium_encoding::fbs::...`
- [x] 2.5 Move `LogRecord`, `LogLevel`, `LogField`, `LogSpan` types + `FlatMsg` impls from `log.rs` into `selium-encoding` (leave the tracing subscriber transport in `selium-guest`)
- [x] 2.6 Add `selium-encoding` to workspace `Cargo.toml` members and `[workspace.dependencies]`
- [x] 2.7 Add `selium-guest` dependency on `selium-encoding`; re-export `FlatMsg`, `HasSchema`, `SchemaDescriptor`, `FieldEncoder`, `LogRecord`, `LogLevel`, `LogField`, `LogSpan`, `encode_typed`, `decode_typed`
- [x] 2.8 Verify `selium-guest` compiles and all existing tests pass

## 3. Create `selium-wire` crate

- [x] 3.1 Scaffold `crates/core/wire/` with `Cargo.toml` (depends on `selium-encoding`, `tokio`, `tokio-util`, `futures`)
- [x] 3.2 Define `MessageTransport` trait composing `AsyncRead + AsyncWrite + Unpin` with `poll_ready`, `poll_peer_closed`, `generation` methods
- [x] 3.3 Move `frame.rs` (`FrameHeader`), `framed.rs` (`FrameCodec`, `FramedRead<M>`, `FramedWrite<M>`) — make them generic over `M: MessageTransport` instead of inner reader/writer types
- [x] 3.4 Move `pubsub.rs` — make `Publisher<T, M>` and `Subscriber<T, M>` generic over `M: MessageTransport`; implement `Sink`/`Stream` against the trait
- [x] 3.5 Move `rpc.rs` — make `RpcClient<Req, Rep, M>`, `RpcConnection<Req, Rep, M>`, `RpcRequest` generic over `M`; replace `yield_now()` with injected `Yielder`; replace `ResourceSender` dependency with `Rendezvous` trait
- [x] 3.6 Define `Rendezvous` trait for connection establishment (client→server `shared_id` passing)
- [x] 3.7 Move `tables.rs` (`LiveTable<K, V, M>`) — make generic over `M: MessageTransport` if it directly accesses transport
- [x] 3.8 Move `io/error.rs` into `selium-wire` as the canonical pattern error types
- [x] 3.9 Add `selium-wire` to workspace `Cargo.toml` members and `[workspace.dependencies]`
- [x] 3.10 Verify `selium-wire` compiles (it doesn't need `selium-guest` or `selium-shm` at all); write a smoke test using a mock transport

## 4. Create `selium-shm` crate

- [x] 4.1 Scaffold `crates/core/shm/` with `Cargo.toml` (depends on `selium-memory`, `selium-wire`, `tokio`)
- [x] 4.2 Move `region.rs` (`ChannelRegion`) into `selium-shm`; replace `SharedRegion::allocate`/`attach` calls with `region_provider().allocate()`/`attach()`
- [x] 4.3 Move `ring_buf.rs` (`RingBuf`) — use global `RegionProvider` instead of direct `SharedRegion`; thread `ResourceKind` through provider call
- [x] 4.4 Move `cursor.rs` verbatim
- [x] 4.5 Move `channels/` (Reader, Writer, BlockingReader, BlockingWriter, WeakReader, WeakWriter) — update to work with the provider-injected `ChannelRegion`
- [x] 4.6 Implement `ShmTransport: MessageTransport` wrapping a `(FramedRead + FramedWrite)` pair over ring channels; implement `poll_ready` via reader generation check, `poll_peer_closed` via `writer_count == 0`, `generation` via ring generation
- [x] 4.7 Implement `ShmRendezvous: Rendezvous` using `ResourceSender`/`ResourceListener` (or a dummy for testing)
- [x] 4.8 Add `selium-shm` to workspace `Cargo.toml` members and `[workspace.dependencies]`
- [x] 4.9 Write tests using `HeapRegionProvider` + `ShmTransport` — verify pubsub, RPC round-trips work through the trait stack

## 5. Slim `selium-guest` to WASM SDK only

- [x] 5.1 Implement `HostcallRegionProvider` wrapping the existing `hostcall_ready(HostcallRequest::AllocRegion/AttachRegion/FreeRegion)` path
- [x] 5.2 Add guest init function that installs `HostcallRegionProvider` globally and registers the mailbox reactor
- [x] 5.3 Remove `io/` module from `selium-guest` (now in `selium-wire` + `selium-shm`)
- [x] 5.4 Remove `memory.rs` (now in `selium-memory`), `encoding.rs`, `codec.rs`, `fbs/` (now in `selium-encoding`), `LogRecord` types (now in `selium-encoding`)
- [x] 5.5 Add `selium-guest` dependencies on `selium-wire` + `selium-shm`; re-export encoding/memory primitives at the crate root. (`selium_guest::io` removed per decision in Phase 5.)
- [x] 5.6 Implement `Rendezvous` for `ResourceSender`/`ResourceListener` in `selium-guest` (the hostcall-backed impl)
- [x] 5.7 Update `net/`, `context.rs`, `resource.rs`, `error.rs` imports to use `selium-shm`/`selium-wire`/`selium-memory`/`selium-encoding` directly
- [x] 5.8 Update `Cargo.toml` features: `io` feature gates `selium-shm` + `selium-wire` deps; `quinn` feature unchanged; `logging` feature unchanged
- [x] 5.9 Verify all existing guest crates (`crates/guests/*`) compile without changes

## 6. Repoint `selium-runtime`

- [x] 6.1 Replace `selium-guest` dependency with `selium-encoding` + `selium-wire` in `crates/core/runtime/Cargo.toml` (`selium-shm`/`selium-memory` added as dev-deps for tests only)
- [x] 6.2 Install runtime's own `RegionProvider` (backed by the existing region table in `Runtime/HostcallHandler`) at startup — **deferred**; not required for current generic flush signature
- [x] 6.3 Fix imports: `selium_guest::io::rpc::RpcClient` → generic `selium_wire::rpc::RpcClient`; `selium_guest::FlatMsg` → `selium_encoding::FlatMsg`; `selium_guest::log::LogRecord` → `selium_encoding::log::LogRecord`
- [x] 6.4 Implement `Rendezvous` for the runtime's own connection establishment (directly into the region table, no hostcall loopback) — **deferred** with 6.2
- [x] 6.5 Verify `selium-runtime` compiles and all tests pass; confirm no `extern "C"` guest hostcall stubs are linked

## 7. Create reference bridge guest

- [x] 7.1 Scaffold `crates/guests/bridge/` as a `cdylib` guest crate depending on `selium-guest`
- [x] 7.2 Implement `main()` or `#[entrypoint]` that initializes QUIC via `selium-guest::net::quinn`, listens for streams
- [x] 7.3 For each accepted QUIC stream, create `QuicTransport` (from `selium-quic`) and relay frames to/from `ShmTransport` rings within the bridge's capability grants
- [x] 7.4 Implement transparent relay: read frame from one transport, write identical frame to the other; preserve correlation tags
- [x] 7.5 Add `crates/guests/bridge/` to workspace members
- [x] 7.6 Verify the bridge compiles to WASM

## 8. Cleanup and verification

- [x] 8.1 Run full workspace build (`cargo build --workspace`) — all crates compile
- [x] 8.2 Run full workspace tests (`cargo test --workspace`) — all tests pass
- [x] 8.3 Run `cargo clippy --workspace` — no warnings
- [x] 8.4 Verify `cargo build --target wasm32-unknown-unknown -p selium-guest` succeeds
- [x] 8.5 Verify `cargo build --target wasm32-unknown-unknown -p selium-bridge-guest` succeeds
- [x] 8.6 Remove any remaining `#[cfg(target_arch = "wasm32")]` branching in `selium-memory`, `selium-encoding`, `selium-wire`, `selium-shm`
- [x] 8.7 Update `ARCHITECTURE.md` or equivalent docs to describe the new crate layering
