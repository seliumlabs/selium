## 1. Shared Region Coordination Layout

- [x] 1.1 Define shared coordination offsets in `selium-guest/src/io/region.rs`: `NEXT_TAIL_OFFSET = 8`, `WRITER_COUNT_OFFSET = 16`, `READER_SLOTS_OFFSET = 24`, `NEXT_WRITER_ID_OFFSET = 1048`, `READER_SLOT_COUNTER_OFFSET = 1056`
- [x] 1.2 Add shared-memory coordination accessors to `ChannelRegion`: `load_next_tail`, `cas_next_tail`, `load_writer_count`, `fetch_add_writer_count`, `load_reader_slot(pos)`, `store_reader_slot(slot, pos)`, `fetch_add_next_writer_id`, `fetch_add_reader_slot_counter`
- [x] 1.3 Each accessor delegates to `RegionMapping`'s existing atomic methods (`atomic_load_u64`, `compare_exchange_u64`, `fetch_add_u64`, `atomic_store_u64`) at the designated offset
- [x] 1.4 Remove `next_tail`, `writer_count`, and `reader_slots` from `ChannelPrivateState`; keep `tail_cache`, `next_mutation_id` in private state
- [x] 1.5 Update `reserve_tail` to CAS on shared `next_tail` via `ChannelRegion::cas_next_tail` instead of private `ChannelPrivateState::next_tail`
- [x] 1.6 Update `minimum_reader_position` to scan shared `reader_slots` via `ChannelRegion::load_reader_slot` instead of private state
- [x] 1.7 Update `allocate_reader_slot` / `update_reader_slot` / `release_reader_slot` to use shared `reader_slots` via `ChannelRegion` methods
- [x] 1.8 Update writer count increment/decrement to use shared `fetch_add_writer_count` via `ChannelRegion`
- [x] 1.9 Update `allocate_writer_id` to use shared `fetch_add_next_writer_id`

## 2. RingBuf and Channel Updates

- [x] 2.1 Update `RingBuf::reserve` to call `region.reserve_tail(len, protect_readers)` which now CAS's against shared `next_tail`
- [x] 2.2 Update `StrongReader::advance` to write position to shared `reader_slots` array
- [x] 2.3 Update `StrongWriter::Drop` and `WeakWriter` creation to use shared `writer_count`
- [x] 2.4 Update `allocate_reader_slot` to use shared `reader_slot_counter` via `fetch_add` for global uniqueness
- [x] 2.5 Run existing `io` tests; fix any failures from shared-memory coordination refactor
- [x] 2.6 Add tests for cross-writer tail reservation contention within a single process (multiple `ChannelRegion` clones simulating multi-writer)

## 3. Context and Discovery

- [x] 3.1 Un-stub `Context::from_raw`: use `ResourceSender::attach(discovery_handle)` to connect to the discovery host queue
- [x] 3.2 Update `Context::lookup` to use `RpcClient::request` once the `selium-rpc` crate is implemented
- [x] 3.3 Update `Context` struct to hold `RpcClient<DiscoveryRequest, DiscoveryResponse>` (requires `selium-rpc` to exist)
- [x] 3.4 Add `RPC_REQ_CAPACITY` and `RPC_REP_CAPACITY` constants back to `context.rs`
- [x] 3.5 Update `from_raw_with_invalid_handle_fails` test to expect the correct error type

## 4. selium-rpc Crate

- [x] 4.1 Create `crates/patterns/rpc/` directory with `Cargo.toml` depending on `selium-guest` and `selium-abi`
- [x] 4.2 Implement `RpcClient<Req, Rep>`: on `connect`, allocate a multi-memory region with request + reply rings, send `shared_id` via `ResourceSender`, attach rings, return client
- [x] 4.3 Implement `RpcClient::request`: rkyv-encode `Req`, write frame to request ring with correlation tag, wait on reply ring generation counter, decode matching reply
- [x] 4.4 Implement `RpcConnection<Req, Rep>`: attach to request + reply rings from `shared_id`, provide `recv()` that returns `RpcRequest`
- [x] 4.5 Implement `RpcRequest::payload`: borrow decoded request from ring buffer frame
- [x] 4.6 Implement `RpcRequest::reply`: rkyv-encode `Rep`, write frame to reply ring with matching correlation tag
- [x] 4.7 Implement `RpcAccept<Req, Rep>`: `Accept` trait impl that attaches to the region from `IncomingConnection`
- [x] 4.8 Update `crates/guests/discovery/Cargo.toml` to depend on `selium-rpc` instead of the old stubs
- [x] 4.9 Verify discovery guest compiles and its tests pass with real RPC types

## 5. Kernel Network Proxy Rewrite

- [x] 5.1 Add shared-memory coordination offset constants to `network_runtime.rs` matching the guest-side layout
- [x] 5.2 Rewrite `create_stream_region` to initialise two ring buffers with the new layout: generation counter, next_tail, writer_count, reader_slots in page 0
- [x] 5.3 Rewrite `proxy_inbound` to CAS on shared `next_tail`, use single-phase write with release fence, bump generation counter, and notify via wasmtiny Store
- [x] 5.4 Rewrite `proxy_outbound` to poll generation counter for new data, read frames via acquire fence, update kernel reader slot in shared `reader_slots`
- [x] 5.5 Rewrite `udp_proxy_recv` and `udp_proxy_send` to use the same shared-memory coordination protocol
- [x] 5.6 Remove old ring buffer code: `decode_frame_header`, `read_frame`, `write_frame`, `reserve_tail`, `read_at`, `write_at`, `update_kernel_reader_slot`, `release_kernel_reader_slot`, `minimum_reader_position`, and all layout constants
- [x] 5.7 Remove `proxy_local_id` pre-attach pattern — kernel already has `Store` access, can read/write shared memory directly via `store.read_shared_region` / `store.write_shared_region`
- [x] 5.8 Wire wasmtiny's `notify_shared_region` (or equivalent) for kernel-to-guest generation counter wake; if not available, `write_shared_region` through the Store should trigger futex wake via the underlying `mmap`
- [x] 5.9 Update legacy tests under `#[cfg(any())]` to compile and pass against new layout

## 6. Guest TCP/UDP Handles

- [x] 6.1 Un-stub `TcpStream::connect`: use `TcpConnect` hostcall, attach returned region, create inbound + outbound `RingBuf` handles
- [x] 6.2 Implement `AsyncRead for TcpStream`: read frames from inbound ring, copy payload to `ReadBuf`, handle EOF when `writer_count` reaches 0
- [x] 6.3 Implement `AsyncWrite for TcpStream`: write frames to outbound ring with single-phase write protocol
- [x] 6.4 Un-stub `TcpStream::attach_shared`: attach to an existing stream region
- [x] 6.5 Implement `TcpAccept::accept`: use `IncomingConnection::shared_id` to attach region, return `TcpStream`
- [x] 6.6 Un-stub `UdpSocket::bind`: use `UdpBind` hostcall, attach returned region, create recv + send `RingBuf` handles
- [x] 6.7 Implement UDP recv: read framed datagrams from recv ring, decode `[addr_len u16][addr bytes][payload]` format
- [x] 6.8 Implement UDP send: encode `[addr_len u16][addr bytes][payload]`, write frame to send ring

## 7. Runtime Hostcall Dispatch Updates

- [x] 7.1 Verify `TcpConnect` dispatch returns `SharedRegionDescriptor` with the new ring buffer layout initialised by the kernel
- [x] 7.2 Verify `UdpBind` dispatch returns `SharedRegionDescriptor` with the new ring buffer layout (remove stub error path)
- [x] 7.3 Verify `TcpBind` dispatch returns `HostQueueDescriptor` unchanged (accept loop already uses `create_stream_region`)
- [x] 7.4 Ensure `HostQueue` hostcalls (`HostQueueCreate`, `HostQueueAttach`, `HostQueueSend`, `HostQueueRecv`) remain dispatched — they were incorrectly marked for removal
- [x] 7.5 Add discovery `shared_id` to application guest entrypoint arguments during bootstrap

## 8. Integration and Cleanup

- [x] 8.1 Remove `guest/src/io/rpc/` module stubs — RPC lives in `selium-rpc` crate now
- [x] 8.2 Remove `guest/src/signal.rs` placeholder — no longer needed
- [x] 8.3 Update `guest/src/lib.rs` exports: remove `io::rpc` module, add `selium-rpc` re-export or note it's a separate crate
- [x] 8.4 Run full workspace build: `cargo build --workspace`
- [x] 8.5 Run full test suite: `cargo test --workspace`
- [x] 8.6 Fix any compilation failures or test regressions across crates

## 9. Tests

- [x] 9.1 Test: two guest-side writers (simulated via separate `ChannelRegion` instances) coordinate on shared `next_tail`, no position collisions
- [x] 9.2 Test: reader in one `ChannelRegion` sees frames written by writer in another `ChannelRegion` instance (same underlying `RegionMapping`)
- [x] 9.3 Test: writer backpressure triggers when shared `reader_slots` minimum position is too close to tail
- [x] 9.4 Test: reader detects EOF when shared `writer_count` reaches 0
- [x] 9.5 Test: `RpcClient::request` / `RpcConnection::recv` / `RpcRequest::reply` round-trip within a single process
- [x] 9.6 Test: `Context::from_raw` with invalid handle returns error
- [x] 9.7 Test: kernel `create_stream_region` creates regions with correct shared-memory coordination layout
- [x] 9.8 Test: kernel proxy writes frames that guest `RingBuf` can read (in-process integration test)
