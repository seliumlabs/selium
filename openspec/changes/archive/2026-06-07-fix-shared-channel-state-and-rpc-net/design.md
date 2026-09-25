## Context

The "dumb host, smart guest" migration (2026-06-03) moved all ring buffer coordination metadata from shared memory into per-guest `ChannelPrivateState`. The shared region was reduced to a generation counter plus raw ring data. This was based on the assumption that ring buffer coordination state doesn't need cross-process visibility.

That assumption is wrong. Channels are designed for many-to-many cross-process communication. With `next_tail`, `reader_slots`, and `writer_count` in per-guest private memory:

- **Multiple writers across guests** each have a private `next_tail` starting at 0 — they collide on the same positions, corrupting data
- **Backpressure is invisible** — a writer in guest A cannot see reader cursors in guest B, so it cannot detect when the ring is full
- **Reader EOF detection is broken** — a reader cannot detect that all writers have disconnected because `writer_count` is private

TCP/UDP and RPC were stubbed during the migration because the guest-side data plane (new `RingBuf`) and the kernel proxy (old `network_runtime.rs`) use incompatible ring buffer layouts.

## Goals / Non-Goals

**Goals:**
- Restore cross-process many-to-many channel coordination by placing `next_tail`, `writer_count`, and `reader_slots` in shared memory
- Implement the `selium-rpc` crate for typed request/reply over shared-memory ring buffers
- Implement TCP and UDP guest handles on top of the fixed `RingBuf`
- Rewrite the kernel network proxy to coordinate through the same shared-memory ring buffer layout that guests use
- Restore `Context::from_raw` and `Context::lookup` for guest-side discovery resolution
- Keep the single-phase write protocol, generation counter notification, and per-page `mprotect` from the previous migration intact

**Non-Goals:**
- Changing the strong/weak reader/writer semantics — those are preserved
- Cross-host or distributed shared memory — this remains local-only
- Live tables (`selium-tables`) — still deferred
- Quinn transport integration — still deferred

## Decisions

### 1. Shared memory layout: coordination fields return to page 0

The shared region layout becomes:

```
Page 0:
  Offset 0:    generation_counter   (u64)  — atomic wait/notify
  Offset 8:    next_tail            (u64)  — writers CAS to reserve space
  Offset 16:   writer_count         (u64)  — writers increment on attach, decrement on drop
  Offset 24:   reader_slots[128]    (128 × u64) — each strong reader's position
  Bytes 1056–4095: reserved / future use
Page 1..N: ring buffer data
```

`next_tail` is atomically CAS'd by writers across all processes. `reader_slots` are atomically updated by each strong reader. `writer_count` is atomically incremented/decremented across processes.

**Fields staying in `ChannelPrivateState`:**
- `tail_cache` — writer-local optimization, not needed across processes
- `next_writer_id` — per-guest allocator, each guest gets a unique range or uses CAS on a shared counter (see Decision 3)
- `next_mutation_id` — per-guest allocator, same reasoning

**Rationale:** This is the minimum set of fields that must be shared for many-to-many channels to work. At ~1056 bytes, it fits easily in page 0 without sacrificing the per-page `mprotect` isolation model (reader cursor pages can still be individually protected starting at page 1).

**Alternatives considered:**
- Keep private state and add a coordinator guest — adds a process, a hop, and a failure domain. Rejected.
- Use a separate shared region for metadata — adds complexity without benefit. Page 0 is already allocated, might as well use it.
- Per-writer rings with fan-in — solves multi-writer but introduces ordering complexity and doubles memory. Rejected.

### 2. Writer ID allocation: shared atomic counter

With `next_tail` and `reader_slots` in shared memory, a writer from any guest can reserve space. But `writer_id` allocation was previously done via a private `next_writer_id` counter. This must move to shared memory too, using `fetch_add` on a u64 at a well-known offset in page 0:

```
Offset 1048: next_writer_id  (u64)  — fetch_add to allocate unique writer IDs
Offset 1056: next_mutation_id (u64) — fetch_add for globally unique mutation IDs
```

**Rationale:** Writer IDs must be globally unique for pub/sub message attribution. A shared atomic counter is the simplest way to achieve this without a central allocator.

### 3. Kernel network proxy: use the unified ring buffer layout

The kernel proxy threads (`proxy_inbound`, `proxy_outbound`, `udp_proxy_recv`, `udp_proxy_send`) currently use a bespoke ring buffer implementation in `network_runtime.rs` with host-mediated two-phase writes, a separate reader slot management system, and no generation counter. This ~900 lines is replaced with code that coordinates through the same shared-memory layout as the guest `RingBuf`.

For each TCP connection, the kernel creates one shared region containing two ring buffers (inbound + outbound), similar to the current `create_stream_region` but writing the new layout:

```
Multi-memory region:
  Header: count=2, entry[0]={offset, len}, entry[1]={offset, len}
  Inbound ring:  page 0 = coordination fields, page 1..N = data
  Outbound ring: page 0 = coordination fields, page 1..N = data
```

The proxy reads/writes frames using the same single-phase write protocol as `RingBuf::write_frame`. It manages a private tail cursor for reservation but CAS's against the shared `next_tail`. It reads the shared `reader_slots` for backpressure.

**Kernel notification of guests:** The kernel can't execute `memory.atomic.notify` (it's a WASM instruction), but wasmtiny provides `notify_shared_region` via its `Store` API. The kernel calls this after bumping the generation counter. If wasmtiny doesn't expose this, the kernel writes to the generation counter via `write_shared_memory` and the guest's `wait32` wake is handled by the underlying futex mechanism (since `write_shared_memory` writes through the same `mmap`).

**Guest notification of kernel:** The kernel proxy polls the generation counter in a loop with `thread::sleep` (already the pattern in the existing proxy code). The guest bumps the counter after writing, which the kernel sees via `read_shared_memory`.

**Rationale:** A single ring buffer layout eliminates the incompatibility between kernel proxy and guest. The kernel already has `mmap` access via wasmtiny's `Store`, so it can atomically CAS on shared fields.

**Alternatives considered:**
- Keep kernel proxy as-is and add a translation layer in the guest — doubles the code paths and defeats the "dumb host" goal. Rejected.
- Have the kernel proxy use a completely different IPC mechanism — rejected; shared memory is already the substrate, use it directly.

### 4. RPC: two-ring design per connection

RPC uses two ring buffers (request + reply) within a single shared region. Connection establishment:

```
Client                                    Server
────────────────────────────────────────────────────
1. alloc_region() → shared_id
2. sender.send(shared_id)  ──HostQueue──→  3. listener.recv() → shared_id
4. attach_region(shared_id)                5. attach_region(shared_id)
6. RpcClient { req_writer, rep_reader }    7. RpcConnection { req_reader, rep_writer }
```

The shared region layout for an RPC session:

```
Multi-memory region:
  Header: count=2
  Request ring:  page 0 = coordination, page 1..N = data  (client writes, server reads)
  Reply ring:    page 0 = coordination, page 1..N = data  (server writes, client reads)
```

Each ring is single-writer by construction, so `next_tail` contention is never an issue for RPC. The shared coordination fields are still present for consistency and backpressure.

`RpcClient::request` encodes the `Req` type via rkyv, writes it as a frame to the request ring, bumps the generation counter, then waits on the reply ring's generation counter (via `memory.atomic.wait32`). `RpcConnection::recv` blocks on the request ring's counter, decodes the request, and returns an `RpcRequest` that can `.reply(response)` by writing to the reply ring.

**Rationale:** Two unidirectional rings avoid the complexity of multiplexing requests and replies on a single ring. The `tag` field in `FrameHeader` carries a correlation ID so the client can match replies to outstanding requests (supporting pipelined requests).

**Alternatives considered:**
- Single ring with message type discrimination — simpler allocation but requires tagging every frame with direction, complicates framing. Rejected.
- RPC over pub/sub channels — adds indirection without benefit. Rejected.

### 5. `next_writer_id` range allocation

Rather than CAS on every writer creation, each guest can `fetch_add` a block of IDs from the shared counter and allocate from its local block. This is an optimization, not required for correctness. For the initial implementation, direct `fetch_add` on the shared counter is simpler and correct.

### 6. TCP stream: shared region with two rings

`TcpStream::connect` calls the `TcpConnect` hostcall, which returns a `SharedRegionDescriptor`. The guest attaches the region and creates two `RingBuf` handles — one for outbound (guest writes → kernel reads → socket) and one for inbound (kernel writes → guest reads → socket data).

`TcpListener::accept` already returns a `TcpStream` via `HostQueue` → `IncomingConnection` → `TcpAccept`. The `IncomingConnection::shared_id` is the region containing the two rings. `TcpAccept::accept` attaches and returns a working `TcpStream`.

`TcpStream` implements `AsyncRead`/`AsyncWrite` by reading/writing frames on the inbound/outbound rings. The `tag` field carries no semantic meaning for streams (it's always 0). The kernel proxy writes received socket data as frames on the inbound ring; the guest reads them. The guest writes outbound data as frames on the outbound ring; the kernel proxy reads and sends them.

### 7. Guest-side `io::RingBuf` changes

`RingBuf::reserve` currently CAS's against `ChannelPrivateState::next_tail`. It changes to CAS against the shared `next_tail` at offset 8 in the shared region. `RingBuf::write_frame`'s backpressure check reads `reader_slots` from shared memory instead of private state. `StrongReader::advance` writes its position to the shared `reader_slots` array.

`RegionMapping` already has `fetch_add_u64`, `compare_exchange_u64`, `atomic_load/store_u64` on the `mmap`'d pointer — these work across processes because the underlying memory is `MAP_SHARED`. No new wasmtiny capabilities needed.

## Risks / Trade-offs

- **Shared `next_tail` contention:** Multiple writers CAS on the same u64. Under high contention, exponential backoff (already implemented) handles this. In practice, RPC rings are single-writer per direction, and TCP streams have exactly one writer per ring. Pub/sub is the only true multi-writer case, and the backoff loop is proven in tests.

- **Reader slot index coordination:** Each guest needs to know which reader slot index it owns. For RPC and TCP, there's exactly one reader on the other side, so slot 0 is implicit. For pub/sub multi-reader, the slot index can be allocated via a shared `reader_slot_counter` (fetch_add on offset 1040 in page 0).

- **Kernel proxy cannot `memory.atomic.wait32`:** The proxy polls the generation counter with `thread::sleep`. For the inbound ring (kernel writes, guest reads), this is fine — the kernel is the writer, it doesn't need to wait. For the outbound ring (guest writes, kernel reads), the kernel polls the generation counter. A 1ms poll interval is acceptable for a proxy thread that's already blocking on real I/O.

- **Migration of existing `ChannelRegion` tests:** Tests that create regions and share them between clones within a single process continue to work — the shared memory atomics are the same operations whether the memory is `mmap`'d or heap-allocated (both go through `RegionMapping`). Cross-process tests won't exist until the runtime integration is done, which is outside this change's scope.

## Open Questions

- Should `next_writer_id` and `next_mutation_id` also move to shared memory, or is per-guest allocation with a shared base sufficient? Leaning toward shared memory for simplicity, but the per-guest block allocation optimization can be added later.
- Does wasmtiny's `Store` expose a `notify_shared_region` method, or does the kernel need to use a different wake mechanism? If not exposed, writing to the generation counter via `write_shared_memory` should trigger the futex wake implicitly since it's the same `mmap`.
