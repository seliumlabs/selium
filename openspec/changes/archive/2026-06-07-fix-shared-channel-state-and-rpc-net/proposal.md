## Why

The "dumb host, smart guest" migration moved cross-process coordination metadata (`next_tail`, `reader_slots`, `writer_count`) from shared memory into per-guest private `ChannelPrivateState`. This breaks channels as a cross-process primitive: multiple writers from different guests collide on `next_tail`, backpressure is invisible across process boundaries, and readers cannot detect writer disconnection. Separately, RPC and TCP/UDP networking were stubbed out during that migration and remain unimplemented against the new shared-memory ring buffer.

## What Changes

- **BREAKING**: Move `next_tail`, `writer_count`, and `reader_slots` from per-guest `ChannelPrivateState` back into the shared memory region, so channels support many-to-many cross-process coordination with proper backpressure
- Keep `tail_cache`, `next_writer_id`, and `next_mutation_id` in per-guest private memory as process-local optimizations
- Implement the `selium-rpc` crate with `RpcClient`, `RpcConnection`, `RpcRequest`, and `RpcAccept` on top of the fixed ring buffer
- Un-stub `TcpStream` and `UdpSocket` guest handles to attach shared regions returned by kernel network hostcalls and read/write via `RingBuf`
- Rewrite kernel network proxy (`proxy_inbound`, `proxy_outbound`, `udp_proxy_recv`, `udp_proxy_send`) to use the unified ring buffer layout with shared-memory coordination fields
- Remove the kernel-side `read_shared_memory`/`write_shared_memory`-based ring buffer implementation in `network_runtime.rs` in favor of directly operating on the shared region layout that guests use
- Un-stub `Context::from_raw` and `Context::lookup` once the RPC client is working

## Capabilities

### New Capabilities
- `selium-rpc`: Typed request/reply protocol over shared-memory ring buffers, providing `RpcClient` and `RpcConnection` for inter-guest communication, with connection handshake via `HostQueue`
- `guest-networking`: TCP stream and UDP socket guest handles backed by the shared-memory ring buffer, with the kernel proxy using the same ring buffer layout as guests

### Modified Capabilities
- `selium-guest`: Ring buffer layout gains shared-memory `next_tail`, `writer_count`, and `reader_slots`; `ChannelPrivateState` retains only process-local fields; `Context::from_raw` and `Context::lookup` return to real implementations
- `selium-abi`: `TcpStream` and `UdpSocket` stubs graduate from error-returning placeholders to fully implemented ABI contracts; RPC types (`RpcClient`/`RpcConnection`/etc.) move from guest-internal stubs to the `selium-rpc` crate
- `selium-kernel`: Network proxy rewritten to coordinate via shared-memory atomics on the unified ring buffer layout, replacing the old host-mediated ring buffer implementation
- `selium-runtime`: Hostcall dispatch for `TcpConnect`, `TcpBind`, and `UdpBind` updated to return regions compatible with the new guest ring buffer layout
- `guest-shared-memory`: Shared region layout now includes `next_tail` (u64), `writer_count` (u64), and `reader_slots` (128 × u64) in page 0 alongside the generation counter

## Impact

| Area | Impact |
|------|--------|
| `selium-guest` | `ChannelPrivateState` shrinks to local-only fields; `ChannelRegion` gains atomic accessors for shared-memory coordination slots; `Context` restored to working RPC-based discovery |
| `selium-abi` | No hostcall surface changes needed — existing `TcpConnect`, `TcpBind`, `UdpBind` variants already return `SharedRegionDescriptor`; stubs graduate to real implementations |
| `selium-rpc` (new) | New crate implementing typed RPC on shared-memory ring buffers, extracted from `selium-guest/src/io/rpc/` |
| `selium-kernel` | `network_runtime.rs` ~900 lines of old ring buffer code replaced with ~300 lines using shared-memory atomic coordination on the unified layout |
| `selium-runtime` | Hostcall dispatch for network hostcalls updated to initialise the new ring buffer layout (generation counter, next_tail, writer_count, reader_slots) |
| `crates/guests/discovery` | Already written against RPC types — works once `selium-rpc` is implemented |
