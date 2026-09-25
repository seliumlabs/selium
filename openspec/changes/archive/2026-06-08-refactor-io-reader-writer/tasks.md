## 1. Safety Documentation

- [x] 1.1 Add safety comments to `unsafe impl Send for RegionMappingInner` explaining WASM shared memory and native Arc-backed safety
- [x] 1.2 Add safety comments to `unsafe impl Sync for RegionMappingInner` with same rationale

## 2. FramedRead / FramedWrite Types

- [x] 2.1 Create `crates/core/guest/src/io/framed.rs` with `FramedRead<R>` struct wrapping `R: AsyncRead + Unpin`
- [x] 2.2 Implement `FramedRead::read_frame(&mut self) -> Result<(Vec<u8>, u32)>` using `AsyncRead::read` and `FrameHeader` decoding
- [x] 2.3 Implement `FramedRead::generation(&self) -> Result<u64>` delegating to inner reader
- [x] 2.4 Create `FramedWrite<W>` struct wrapping `W: AsyncWrite + Unpin`
- [x] 2.5 Implement `FramedWrite::write_frame(&mut self, payload: &[u8], tag: u32) -> Result<()>` using `AsyncWrite::write_all` and `FrameHeader` encoding
- [x] 2.6 Add `pub mod framed;` to `io/mod.rs` and re-export `FramedRead`, `FramedWrite`

## 3. Refactor Reader / Writer to Byte-Stream with AsyncRead/AsyncWrite

- [x] 3.1 Refactor `StrongReader` → `Reader`: remove `read()` method returning `(Vec<u8>, u32)`, add internal frame-reading logic in `poll_read`
- [x] 3.2 Implement `tokio::io::AsyncRead` for `Reader` — `poll_read` reads frame payloads, copies bytes to caller buffer, tracks generation counter, returns `Error::Overwritten` via `io::Error`
- [x] 3.3 Implement `tokio::io::AsyncRead` for `WeakReader` — same as `Reader` but without reader_slot management
- [x] 3.4 Refactor `StrongWriter` → `Writer`: remove `write()` method, add frame-writing logic in `poll_write`
- [x] 3.5 Implement `tokio::io::AsyncWrite` for `Writer` — `poll_write` creates frames with `tag = 0`, uses `protect_readers = true`
- [x] 3.6 Implement `tokio::io::AsyncWrite` for `WeakWriter` — `poll_write` creates frames with `tag = 0`, uses `protect_readers = false`
- [x] 3.7 Add `poll_ready(&mut self) -> Result<bool>` to `Reader` and `WeakReader` (delegates to internal frame-ready check, needed by FramedRead)
- [x] 3.8 Add `generation(&self) -> Result<u64>` method to `Reader` (delegates to `ChannelRegion::load_generation`)
- [x] 3.9 Remove `Reader` and `Writer` enums (the polymorphic wrappers); rename to `AnyReader`/`AnyWriter` or remove entirely if unused

## 4. Upgrade / Downgrade on Reader / Writer

- [x] 4.1 Implement `Reader::downgrade(self) -> WeakReader` — releases reader_slot, returns WeakReader at same position
- [x] 4.2 Implement `WeakReader::upgrade(self) -> Result<Reader>` — allocates reader_slot at current position
- [x] 4.3 Implement `Writer::downgrade(self) -> WeakWriter` — decrements writer_count, returns WeakWriter with same writer_id
- [x] 4.4 Implement `WeakWriter::upgrade(self) -> Result<Writer>` — increments writer_count, returns Writer with same writer_id
- [x] 4.5 Fix `writer_from_ring` in pubsub.rs: remove `increment_writer_count()` call (WeakWriter should not manage writer_count; Writer handles it in constructor)
- [x] 4.6 Move `increment_writer_count()` into `Writer::new()` constructor, keep `WeakWriter::new()` without count management

## 5. Update Channel Factory

- [x] 5.1 Update `Channel::strong_reader()` to return `Reader` (new concrete type) instead of `StrongReader`
- [x] 5.2 Update `Channel::weak_reader()` to return `WeakReader`
- [x] 5.3 Update `Channel::strong_writer()` to return `Writer` — move increment_writer_count inside Writer::new
- [x] 5.4 Update `Channel::weak_writer()` to return `WeakWriter`
- [x] 5.5 Update `channels/mod.rs` re-exports

## 6. Update Pub/Sub with FramedRead / FramedWrite

- [x] 6.1 Update `Publisher<T>` to store `FramedWrite<WeakWriter>` instead of `WeakWriter`
- [x] 6.2 Update `Publisher::publish()` to use `self.writer.write_frame(encoded, 0)` instead of `self.writer.write(&bytes)`
- [x] 6.3 Update `Subscriber<T>` to store `FramedRead<WeakReader>` instead of `WeakReader`
- [x] 6.4 Remove `Subscriber::check_overwritten` — overwrite detection now handled by `Reader::poll_read` via `FramedRead`
- [x] 6.5 Update `Subscriber::read_with_writer_id()` to use `self.reader.read_frame()` instead of `self.reader.read()`
- [x] 6.6 Update `Subscriber`'s `Stream` impl `poll_next` to use `read_frame()` and remove inline `check_overwritten` call
- [x] 6.7 Implement `Subscriber::upgrade(self) -> Result<Subscriber<T>>` — upgrades inner `WeakReader` to `Reader`
- [x] 6.8 Implement `Subscriber::downgrade(self) -> Subscriber<T>` — downgrades inner `Reader` to `WeakReader`
- [x] 6.9 Implement `Publisher::upgrade(self) -> Result<Publisher<T>>` — upgrades inner `WeakWriter` to `Writer`
- [x] 6.10 Implement `Publisher::downgrade(self) -> Publisher<T>` — downgrades inner `Writer` to `WeakWriter`
- [x] 6.11 Update `reader_from_ring` and `writer_from_ring` helpers for new type signatures

## 7. Fold RPC into selium-guest

- [x] 7.1 Copy `crates/patterns/rpc/src/lib.rs` to `crates/core/guest/src/io/rpc.rs`
- [x] 7.2 Copy `crates/patterns/rpc/src/error.rs` into `io/rpc.rs` (or `io/error.rs` if merging error types)
- [x] 7.3 Refactor `RpcClient` to use `FramedWrite` for request ring and `FramedRead` for reply ring instead of raw `RingBuf` operations
- [x] 7.4 Refactor `RpcConnection` to use `FramedRead` for request ring and `FramedWrite` for reply ring instead of raw `RingBuf` operations
- [x] 7.5 Refactor `attach_rpc_region` and `create_rpc_region` helpers to return `(FramedRead<Reader>, FramedWrite<Writer>)` or similar pairs
- [x] 7.6 Add `pub mod rpc;` to `io/mod.rs` and re-export RPC types
- [x] 7.7 Update `Cargo.toml` workspace: remove `crates/patterns/rpc` from members, remove `selium-rpc` from `[workspace.dependencies]`
- [x] 7.8 Delete `crates/patterns/rpc/` directory

## 8. Deduplicate LiveTable

- [x] 8.1 Verify `crates/core/guest/src/io/tables.rs` is the canonical version (with rkyv derives on `LiveTableMessage`)
- [x] 8.2 Update `io/tables.rs` to use `FramedRead`/`FramedWrite` internally instead of direct `WeakReader`/`WeakWriter` access
- [x] 8.3 Update `Cargo.toml` workspace: remove `crates/patterns/tables` from members, remove `selium-tables` from `[workspace.dependencies]`
- [x] 8.4 Delete `crates/patterns/tables/` directory
- [x] 8.5 Add `pub mod tables;` to `io/mod.rs` if not already present

## 9. Update Context to Use RpcClient

- [x] 9.1 Refactor `Context::from_raw` to construct an `RpcClient<DiscoveryRequest, DiscoveryResponse>` instead of manual `RingBuf` setup
- [x] 9.2 Replace `Context::lookup`'s inline RPC logic (frame writes, generation polling, writer_count checks) with `self.client.request(DiscoveryRequest::Resolve(uri.to_string())).await`
- [x] 9.3 Remove `Context` fields `request_ring`, `reply_ring`, `next_correlation` — replaced by `RpcClient`
- [x] 9.4 Remove constants `RPC_REP_CAPACITY`, `RPC_REQ_CAPACITY` if they become unused
- [x] 9.5 Add `rkyv` encode/decode bounds for `DiscoveryRequest`/`DiscoveryResponse` if not already present

## 10. Update TcpStream to Use Reader / Writer

- [x] 10.1 Replace `TcpStream` fields `inbound: RingBuf` and `outbound: RingBuf` with `reader: Reader` and `writer: Writer`
- [x] 10.2 Replace `TcpStream::poll_read` body with delegation to `self.reader.poll_read`
- [x] 10.3 Replace `TcpStream::poll_write` body with delegation to `self.writer.poll_write`
- [x] 10.4 Replace `TcpStream::poll_flush` and `poll_shutdown` with delegation to `Writer`
- [x] 10.5 Remove `read_pos`, `read_buf`, `read_offset`, `eof` fields — now managed by `Reader`
- [x] 10.6 Update `TcpStream::from_mapping` and `TcpStream::from_rings` to construct `Reader`/`Writer` instead of raw `RingBuf`
- [x] 10.7 Update `parse_dual_ring_region` to return `(Reader, Writer)` or `(RingBuf, RingBuf)` (Reader/Writer can be constructed from RingBuf by caller)

## 11. Implement QuinnUdpSender / QuinnUdpSocket Stubs

- [x] 11.1 Implement `QuinnUdpSender::poll_send`: encode destination address + payload into frame, reserve space on send ring via `RingBuf::reserve`, write frame via `RingBuf::write_frame`
- [x] 11.2 Handle `BufferFull` in `poll_send` → return `Poll::Pending`
- [x] 11.3 Implement `QuinnUdpSocket::poll_recv`: check recv ring for ready frames, decode source address and payload, copy to Quinn buffers, populate `RecvMeta`
- [x] 11.4 Handle empty recv ring in `poll_recv` → return `Poll::Pending` (with waker registration)
- [x] 11.5 Handle `writer_count == 0` in `poll_recv` → return `io::Error` for closed channel
- [x] 11.6 Remove TODO comments from `quinn.rs`

## 12. Update Consumer Imports

- [x] 12.1 Update `crates/guests/external-api/` imports: `selium_rpc::*` → `selium_guest::io::rpc::*`, `selium_tables::*` → `selium_guest::io::tables::*`
- [x] 12.2 Update `crates/guests/scheduler/` imports
- [x] 12.3 Update `crates/guests/discovery/` imports
- [x] 12.4 Update `crates/guests/cluster/` imports
- [x] 12.5 Update `crates/guests/supervisor/` imports
- [x] 12.6 Update any other crate referencing `selium-rpc` or `selium-tables`

## 13. RegionProt Cleanup

- [x] 13.1 Verify `selium_abi::RegionProt` and `wasmtiny::RegionProt` variants remain 1:1
- [x] 13.2 Add a doc comment to `to_wasm_prot` explaining the conversion rationale (ABI crate independence from wasmtiny)

## 14. Final Validation

- [x] 14.1 Run `cargo build --workspace` and fix all compilation errors
- [x] 14.2 Run `cargo test --workspace` and fix all test failures
- [x] 14.3 Run `cargo clippy --workspace` and fix all warnings (especially `undocumented_unsafe_blocks`)
- [x] 14.4 Verify no remaining references to `selium-rpc` or `selium-tables` crate names in any Cargo.toml
- [x] 14.5 Verify `crates/patterns/` directory is fully removed
- [x] 14.6 Run `cargo doc --workspace --no-deps` and verify documentation builds without broken links
