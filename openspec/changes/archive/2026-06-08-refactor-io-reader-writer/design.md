## Context

The current I/O type hierarchy has three issues: fragmentation (every consumer bypasses `StrongReader`/`StrongWriter` to work with `RingBuf` directly), duplication (`LiveTable` in both `io/tables.rs` and `crates/patterns/tables/`, RPC framing logic in both `patterns/rpc/` and `context.rs`), and missing trait implementations (no `AsyncRead`/`AsyncWrite` on reader/writer types, no upgrade/downgrade).

The root cause is a circular dependency: `selium-rpc` depends on `selium-guest` (for `RingBuf`, `ChannelRegion`), so `selium-guest` cannot use `RpcClient` without creating a cycle. This forced `Context` to reimplement RPC framing inline. Similarly, `selium-tables` duplicates `LiveTable` from `io/tables.rs`.

The shared-memory ring buffer infrastructure is mature: cross-process coordination via atomic CAS on `next_tail`, reader backpressure via shared `reader_slots`, EOF detection via `writer_count`, and generation-counter-based notification. These primitives are stable and do not change in this design.

## Goals / Non-Goals

**Goals:**
- Establish a clean three-layer I/O stack: byte-stream → framing → typed patterns
- Implement `AsyncRead`/`AsyncWrite` on `Reader`/`Writer` so they compose with the Tokio ecosystem
- Provide `FramedRead`/`FramedWrite` as an explicit framing layer for pub/sub and RPC
- Eliminate code duplication by folding pattern crates into `selium-guest`
- Resolve the circular dependency so `Context` can use `RpcClient` directly
- Add upgrade/downgrade paths between strong and weak handles
- Fill Quinn integration stubs
- Add missing safety documentation

**Non-Goals:**
- Changing the shared-memory ring buffer protocol (frame format, coordination fields, single-phase write protocol)
- Changing `RingBuf`'s API (it remains the low-level primitive)
- Removing the `Channel` struct (it stays as a factory for reader/writer handles)
- Implementing unframed write mode in the ring buffer (byte-stream writes still use frames internally; the 12-byte `FrameHeader` overhead per write is accepted for now)
- Changing `RegionProt` beyond the `to_wasm_prot` conversion (the enum stays in `selium-abi`)

## Decisions

### Decision 1: Three-layer I/O stack

```
Layer 1: RingBuf (shared-memory ring buffer)
         └─ Atomic operations, CAS on next_tail, read/write bytes

Layer 2: Reader/Writer (byte-stream over RingBuf)
         └─ AsyncRead/AsyncWrite, hides frame headers internally
         └─ Strong variants track reader_slots / writer_count
         └─ Weak variants skip tracking for lower overhead

Layer 3: FramedRead/FramedWrite (explicit framing)
         └─ Wraps any AsyncRead/AsyncWrite
         └─ read_frame() → (Vec<u8>, tag)
         └─ write_frame(payload, tag)
         └─ Used by Subscriber/Publisher and RpcClient/RpcConnection

         FramedRead<WeakReader> ──► Subscriber<T> (+ rkyv decode)
         FramedWrite<WeakWriter> ──► Publisher<T> (+ rkyv encode)
         FramedRead<Reader> + FramedWrite<Writer> ──► RpcClient / RpcConnection
```

**Rationale**: Separating byte-stream from framing makes `Reader`/`Writer` directly usable for bulk data (video, audio, large binaries) without frame-level API noise, while `FramedRead`/`FramedWrite` provide the tag-based correlation that pub/sub and RPC need. This replaces the current situation where every consumer duplicates frame header handling.

**Alternative considered**: Keep framing in `Reader`/`Writer` and provide an `unframed()` mode. Rejected because it makes the common byte-stream case carry framing baggage (return type is always `(Vec<u8>, u32)` even when tags aren't needed).

### Decision 2: Reader/Writer implement AsyncRead/AsyncWrite directly (not via wrapper)

The concrete `Reader`, `WeakReader`, `Writer`, and `WeakWriter` types directly implement `tokio::io::AsyncRead` and `tokio::io::AsyncWrite`. No intermediate wrapper type.

**Rationale**: Avoids an extra type layer. The strong/weak distinction is already captured by the type itself. If a consumer needs polymorphism, they can use `Box<dyn AsyncRead>` or generics.

**Alternative considered**: A generic `ByteReader<R: RingOps>` / `ByteWriter<W: RingOps>` with strong/weak as type parameters. Rejected as over-abstracted — there are only two variants and they share an implementation substrate (`ChannelRegion`), not an abstract ring interface.

### Decision 3: FrameHeader encoding lives in FramedRead/FramedWrite, not in Reader/Writer

`Reader::poll_read` reads frame payload bytes and copies them to the caller's buffer. It handles the `FrameHeader` internally (checking READY flag, skipping the header bytes) but does not expose frames to the caller. `Writer::poll_write` writes the caller's bytes as a single frame (header + payload) with `tag = 0`.

`FramedRead::read_frame` calls `AsyncRead::read` on the inner reader to get payload bytes, then pairs them with the tag (which it reads from the frame header itself). `FramedWrite::write_frame` writes the header and payload together.

**Rationale**: Single responsibility. `Reader`/`Writer` own the byte-stream contract; `FramedRead`/`FramedWrite` own the framing contract. This avoids the awkward situation where `Reader::read` returns `(Vec<u8>, u32)` but most callers ignore the tag.

**Alternative considered**: Have `Reader::read` return `(Vec<u8>, u32)` always, and add a separate `read_bytes` method. Rejected because `AsyncRead` is the standard Rust async byte-stream trait — we want to implement it, and it doesn't carry tags.

### Decision 4: Writer writes one frame per poll_write call

Each `Writer::poll_write(buf)` call creates one frame containing the full `buf`. No internal buffering or batching.

**Rationale**: Simplicity. This matches the current `TcpStream` behavior and avoids introducing a flush policy. Batching can be added later without breaking the `AsyncWrite` contract — the writer would buffer internally and flush on `poll_flush` or when the buffer reaches a threshold. The 12-byte `FrameHeader` overhead per write is acceptable for the initial implementation.

### Decision 5: Upgrade/downgrade consume self

```rust
impl Reader {
    fn downgrade(self) -> WeakReader { ... }  // releases reader_slot
}
impl WeakReader {
    fn upgrade(self) -> Result<Reader> { ... }  // allocates reader_slot at current pos
}
impl Writer {
    fn downgrade(self) -> WeakWriter { ... }  // decrements writer_count (compensates for loss of Drop decrement)
}
impl WeakWriter {
    fn upgrade(self) -> Result<Writer> { ... }  // increments writer_count
}
```

**Rationale**: Consuming `self` prevents double-drop bugs. The old handle is gone; the new handle has the correct Drop behavior. For `Writer::downgrade`, we call `decrement_writer_count()` before returning the `WeakWriter` because `Writer::Drop` would have done so, but `WeakWriter::Drop` does not. This keeps the writer_count accurate.

**Alternative considered**: `&mut self` methods that mutate in place. Rejected because it creates an ambiguous state during the transition (e.g., has the reader_slot been released yet?) and risks double-free on panic.

### Decision 6: Subscriber/Publisher upgrade/downgrade return new handles

```rust
impl<T> Subscriber<T> {
    fn upgrade(self) -> Result<Subscriber<T>> { ... }
    fn downgrade(self) -> Subscriber<T> { ... }
}
```

Internally, `Subscriber<T>` stores either `FramedRead<Reader>` or `FramedRead<WeakReader>`. Upgrade/downgrade swap the inner reader and return a new `Subscriber`.

**Rationale**: Keeps the `Subscriber`/`Publisher` API surface small. The consumer doesn't need to know about `FramedRead`/`FramedWrite` unless they want to. For advanced use, `Subscriber::into_reader()` and `Publisher::into_writer()` expose the inner handle.

### Decision 7: Fold patterns/ into guest/src/io/, re-export from selium-guest

```
crates/core/guest/src/io/
├── rpc.rs          ← was crates/patterns/rpc/src/lib.rs
├── tables.rs       ← existing, keep this version (with rkyv derives)
├── pubsub.rs       ← existing, updated to use FramedRead/FramedWrite
├── channels/
│   ├── reader.rs   ← refactored to byte-stream
│   ├── writer.rs   ← refactored to byte-stream
│   └── mod.rs
├── frame.rs        ← existing, FrameHeader (+ FramedRead/FramedWrite added here)
├── framed.rs       ← NEW: FramedRead, FramedWrite
└── ...
```

Workspace `Cargo.toml`: remove `crates/patterns/rpc` and `crates/patterns/tables` from `members`. Remove `selium-rpc` and `selium-tables` from `[workspace.dependencies]`.

Guest crates that previously depended on `selium-rpc` or `selium-tables` now depend only on `selium-guest` and import from `selium_guest::io::rpc` and `selium_guest::io::tables`.

**Rationale**: The circular dependency dissolves because `RpcClient` now lives in `selium-guest` itself. `Context` can use it directly. `LiveTable` lives in one place.

### Decision 8: Overwritten detection via poll_read error

`Reader::poll_read` (and `WeakReader::poll_read`) track the generation counter and detect overwrite. When the generation delta exceeds ring capacity, `poll_read` returns `Poll::Ready(Err(io::Error::new(io::ErrorKind::Other, Error::Overwritten)))`.

`Subscriber` no longer has its own `check_overwritten` — it relies on the underlying `FramedRead`'s inner reader to surface the error through the `AsyncRead` chain.

**Rationale**: The overwrite condition is a reader-level concern (it's about the ring buffer position, not pub/sub specifically). Pushing it into `Reader::poll_read` means all consumers get it for free, including `FramedRead` and anything built on top.

### Decision 9: Safety comments for RegionMappingInner

Add a block comment above the unsafe impls explaining:
1. In WASM mode, the raw pointer comes from shared linear memory that remains valid for the guest's lifetime
2. In native mode, the pointer is into an `Arc<Vec<u8>>` held by `_backing`, keeping the allocation alive
3. Both modes are `Send + Sync` safe because all access goes through atomic operations at well-known offsets

## Risks / Trade-offs

- **[Breaking API change]** All consumers of `selium-guest`, `selium-rpc`, and `selium-tables` need import path updates. → Mitigation: This is a prototype with a small, controlled set of consumers (the system guest crates). Changes are mechanical find-and-replace.
- **[Frame overhead for byte streams]** Each `poll_write` creates a 12-byte `FrameHeader`. For large writes this is negligible; for tiny writes it's proportionally expensive. → Mitigation: Accept for now. Future optimization can add internal buffering to `Writer` without changing the `AsyncWrite` contract.
- **[Generation counter for overwrite detection in Reader]** `Reader` must track `last_generation` and compare on every `poll_read`. This adds an atomic load per read. → Mitigation: The generation counter is already loaded for notification checks; the additional comparison is cheap. No extra atomic op needed.
- **[Quinn integration complexity]** Bridging Quinn's poll-based `UdpSender`/`AsyncUdpSocket` traits with the async `UdpSocket` methods requires buffering. → Mitigation: Implement against `RingBuf` directly (matching the TcpStream pattern), avoiding the impedance mismatch with async methods. The Quinn types own their own `RingBuf` handles for send/recv.

## Open Questions

None — all design decisions above are settled per discussion with the project lead.
