## 1. FrameHeader Migration

- [x] 1.1 Update `FrameHeader` in `selium-io/src/frame.rs`: change layout from 8 bytes (`len: u32, flags: u16, writer_id: u16`) to 12 bytes (`len: u32, tag: u32, flags: u8, _reserved: [u8; 3]`). Update `ENCODED_SIZE` to 12. Rename `writer_id` to `tag` in all methods (encode, decode, construction).
- [x] 1.2 Update all `FrameHeader` consumers in `selium-io/src/ring_buf.rs`: rename `writer_id` to `tag` in `write_frame` signature and callers.
- [x] 1.3 Update all `FrameHeader` consumers in `selium-io/src/channels/writer.rs`: rename `writer_id` field references to `tag`. Update `StrongWriter` and `WeakWriter` write calls.
- [x] 1.4 Update all `FrameHeader` consumers in `selium-io/src/channels/reader.rs`: rename `writer_id` field references to `tag`.
- [x] 1.5 Update `selium-io/src/pubsub.rs`: rename `writer_id()` method and all references to `tag`. The `TypedSubscriber::read_with_writer_id` method becomes `read_with_tag`.
- [x] 1.6 Update `selium-io/src/tables.rs`: update `LiveTable` and `sync_until_own_mutation` to use `tag` instead of `writer_id`.
- [x] 1.7 Update frame header tests in `selium-io/src/frame.rs` to match the new 12-byte layout and `tag` field.
- [x] 1.8 Run `cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace --all-targets` to verify the migration compiles and all existing tests pass.

## 2. SharedRegionBuilder

- [x] 2.1 Add `SharedRegionBuilder` to `selium-io/src/region.rs` with `new(capacity)`, `add_memory(len)`, and `seal()` methods. The builder SHALL track sub-memories with 8-byte alignment padding and write the layout header on seal.
- [x] 2.2 Add `memory_count()` and `memory(index)` methods to `SharedRegion` for discovering sub-memories after attach. The header SHALL be readable by any party that attaches via `shared_id`.
- [x] 2.3 Add `memory_layout_header` encoding/decoding: magic value, capacity, memory count, and per-memory `(offset, len)` pairs. The header SHALL be at the start of the shared region before sub-memory data.
- [x] 2.4 Write tests for `SharedRegionBuilder`: building a region with two sub-memories, sealing prevents further modifications, out-of-bounds memory access returns an error, and attaching party can discover the correct layout.

## 3. RPC Module

- [x] 3.1 Create `selium-io/src/rpc/mod.rs` with module structure for `accept`, `client`, `connection`, `error`, and `frame` submodules.
- [x] 3.2 Implement `RpcClient<Req, Rep>` in `selium-io/src/rpc/client.rs`: `attach(region)` constructor, `request(payload) -> Result<Rep>` async method that assigns correlation IDs, writes to the request ring, and awaits the matching reply.
- [x] 3.3 Implement `RpcConnection<Req, Rep>` in `selium-io/src/rpc/connection.rs`: `for_server(region, client_pid)` constructor, `recv() -> Result<RpcRequest<Req, Rep>>` async method that reads from the request ring and deserialises the request.
- [x] 3.4 Implement `RpcRequest<Req, Rep>` in `selium-io/src/rpc/connection.rs`: `into_payload()` method, `reply(response) -> Result<()>` async method that writes to the reply ring with the correlation ID and consumes `self`.
- [x] 3.5 Implement `Accept` trait, `AcceptError`, and `RpcAccept<Req, Rep>` in `selium-io/src/rpc/accept.rs`.
- [x] 3.6 Implement `ResourceListener` in `selium-io/src/rpc/accept.rs`: `new(handle)` constructor and `accept::<A: Accept>()` async method. **Moved to `selium-guest/src/resource.rs`**.
- [x] 3.7 Implement `ResourceSender` in `selium-io/src/rpc/accept.rs`: `attach(handle)` and `send(value) -> Result<()>` async method. **Moved to `selium-guest/src/resource.rs`**.
- [x] 3.8 Implement `IncomingConnection` struct with `client_process_id: ProcessId` and `shared_id: u64` fields. **Moved to `selium-guest/src/resource.rs`**.
- [x] 3.9 Define `RpcError` and `AcceptError` types in `selium-io/src/rpc/error.rs` with variants for `ConnectionClosed`, `InvalidRegion`, `LayoutMismatch`, and `BufferFull`/`BufferEmpty`.
- [x] 3.10 Write unit tests for `RpcClient` and `RpcConnection` using in-memory shared regions (native test fallback). **Added `attach_rejects_invalid_region` and `for_server_rejects_invalid_region` tests in selium-io, plus host queue integration tests in selium-runtime.**

## 4. ABI Hostcall Variants

- [x] 4.1 Add `HostQueueSend { handle: u64, value: u64 }` and `HostQueueRecv { handle: u64 }` variants to `HostcallRequest` in `selium-abi/src/lib.rs`. **Updated to use `local_id` instead of raw `handle`**.
- [x] 4.2 Add `ConnectionInfo { client_process_id: ProcessId, value: u64 }` variant to `HostcallOutput` in `selium-abi/src/lib.rs`.
- [x] 4.3 Add `HostQueueCreate` and `HostQueueAttach { shared_id }` to `HostcallRequest`, `HostQueue(HostQueueDescriptor)` to `HostcallOutput`, `HostQueue` capability and resource class, and `HostQueueDescriptor` struct in `selium-abi/src/lib.rs`.
- [x] 4.4 Update `selium-abi` tests to cover the new hostcall variants.

## 5. Guest Context and Entrypoint

- [x] 5.1 Define `Context` struct with `discovery: RpcClient<DiscoveryRequest, DiscoveryResponse>` field and `from_raw(discovery_handle: u64) -> Result<Self>` constructor. **Placed in `selium-io/src/rpc/context.rs` because `selium-guest` cannot depend on `selium-io` (would create circular dependency).**
- [x] 5.2 Define `ResourceSender` and `ResourceListener` handle types in `selium-guest` that wrap hostcalls `HostQueueSend` and `HostQueueRecv`. **Implemented in `selium-guest/src/resource.rs` with `HostQueueDescriptor` via `HostQueueCreate`/`HostQueueAttach`**.
- [x] 5.3 Update `#[entrypoint]` macro in `selium-guest-macros` to accept functions with an optional `Context` parameter. The macro SHALL decode the `Context` from the entry payload when present.
- [x] 5.4 Write tests for `Context::from_raw` and `ResourceSender`/`ResourceListener` using native test fallbacks.

## 6. Runtime Hostcall Implementation

- [x] 6.1 Implement `HostQueueSend` dispatch in `selium-runtime/src/hostcall.rs`: validate the calling guest's capability to reach the target service, enqueue the connection info, and return success or `AbiErrorCode::CapabilityDenied`.
- [x] 6.2 Implement `HostQueueRecv` dispatch in `selium-runtime/src/hostcall.rs`: return the next pending connection entry for the server's listener, or return a pending completion state if no connections are waiting.
- [x] 6.3 Implement connection queue storage in `selium-runtime`: per-listener queue of `IncomingConnection` entries, with async wake notification for the server guest. **Implemented in `selium-kernel` with `HostQueueState` and `tokio::sync::Notify`**.
- [x] 6.4 Implement entrypoint argument passing in `selium-runtime` bootstrap: detect `i64` parameter, decode arguments from `SystemGuestDescriptor::arguments`, and pass them to the guest entrypoint. The discovery guest receives its `HostQueue` `shared_id` via this mechanism.
- [x] 6.5 Write integration tests for hostcall dispatch: authorised send succeeds, unauthorised send returns capability error, recv returns connection info.

## 7. Discovery Wiring

- [x] 7.1 Define `DiscoveryRequest` and `DiscoveryResponse` message types in `selium-discovery` with rkyv derive. `DiscoveryRequest` SHALL include `Resolve(String)` and other variants. `DiscoveryResponse` SHALL include `Found(ResourceTarget)` and `NotFound`. **Placed in `selium-abi` to avoid circular dependency; re-exported from `selium-guest` and used in `selium-discovery`.**
- [x] 7.2 Wire `discovery_main` entrypoint to accept `Context`, extract `ResourceListener`, and accept incoming RPC connections in a loop.
- [x] 7.3 Implement request handling: deserialise `DiscoveryRequest`, route to `DiscoveryStore`, serialise `DiscoveryResponse`, reply via `RpcRequest::reply`.
- [x] 7.4 Write tests for discovery RPC: resolve exact, resolve prefix, not found.

## 8. Final Validation

- [x] 8.1 Run `cargo test --workspace --all-targets` and fix any issues. **All 102 tests pass**.
- [x] 8.2 Run `cargo clippy --workspace --all-targets -- -D warnings` and fix any issues. **Clean pass.**
- [x] 8.3 Verify all spec scenarios have corresponding test coverage.