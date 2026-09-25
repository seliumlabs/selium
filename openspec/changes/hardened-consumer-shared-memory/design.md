## Context

Today, all guests — publishers and consumers alike — attach shared memory regions with full read-write access. A consumer (`Subscriber`, `BlockingReader`, or `Reader`) holds a `ChannelRegion` that wraps a `RegionMapping` with a raw `*mut u8` pointer. While the safe API doesn't expose write methods to the consumer directly, `region()` is public on both `Reader` and `BlockingReader`, giving any consumer direct access to `write_data()`, `bump_generation()`, `write_next_tail()`, `cas_next_tail()`, and all other mutation methods.

The existing `reader_slot` mechanism in wasmtiny provides page-level protection: attach a region with `reader_slot: Some(n)` and page `n` becomes writable while all other pages stay `PROT_READ`. However, this is insufficient for two reasons:

1. In the current layout, reader slots (offset 24) share page 0 with `next_tail` (8), `generation_counter` (0), `writer_count` (16), and other mutable metadata. Making page 0 writable opens all of it.
2. Even if reader slots were on their own page, all 128 slots share that page. One malicious consumer could corrupt another consumer's position.

The fix: consumers get `PROT_READ` for everything, and position updates go through hostcalls where the runtime validates ownership.

**Current state:**
- `wasmtiny` supports `RegionProt::ReadOnly` with per-page `reader_slot` punching
- `wasmtiny`'s `SharedMemoryRegistry` allows host-side writes to shared regions
- `selium-runtime` already boots WASM guests and dispatches hostcalls
- `selium-guest` has hand-written offset constants (`READER_SLOTS_OFFSET = 24`, etc.)
- The `HostCaller` struct in wasmtiny has no caller identity field

**Constraints:**
- WASM page protection works at page granularity (64 KiB in wasmtiny) — cannot protect individual bytes within a page
- The read path must stay zero-copy — consumers must still read from shared memory directly
- The slot lifecycle is dynamic — consumers can join/leave at any time
- Publishers must retain direct read-write access (this change is consumer-scoped only)

## Goals / Non-Goals

**Goals:**
- Prevent a compromised consumer from corrupting ring buffer data, coordination metadata, or other consumers' positions
- Achieve byte-level write granularity for consumer position updates without wasting pages
- Keep the read path zero-copy for consumers (reads remain direct from shared memory)
- Manage reader slot allocation, ownership, and garbage collection in the trusted runtime
- Make the memory layout self-describing and version-validated at attach time
- Provide a generic slot hostcall API that is not I/O-domain-specific

**Non-Goals:**
- Restricting publishers — they remain trusted and keep direct read-write access
- Removing the legacy `reader_slot` mechanism from wasmtiny — it stays for backward compat
- Generalising `SharedTable<T>` as a guest-facing type — deferred to a future change
- Heartbeat-based stale slot detection — process-death GC is sufficient for day 1
- Writer slot hostcall path — this change is consumer-only

## Decisions

### 1. Consumers get PROT_READ, no writable pages

**Decision:** Consumer guests attach with `RegionProt::ReadOnly` and `reader_slot: None`. The runtime enforces this based on the guest's capability grants. All writes from consumers to shared memory trap.

**Rationale:** This eliminates the entire class of direct-memory-write attacks from consumers. The read path remains zero-copy — consumers still execute native WASM load instructions against the shared mapping.

**Alternative considered:** Keep the `reader_slot` mechanism but restructure pages so reader slots are isolated on their own page.
- Rejected because all 128 slots share the same page, so one consumer could still corrupt another's position.

### 2. Slot writes go through generic hostcalls, not domain-specific ones

**Decision:** Three hostcalls — `alloc_slot(region_id, table_offset, elem_size, elem_count)`, `write_slot(region_id, table_offset, elem_size, slot_index, value)`, `free_slot(region_id, table_offset, elem_size, slot_index)`. The runtime does not know this is a "reader position" — it only knows table offsets, element sizes, and ownership.

**Rationale:** The memory layer should not encode I/O semantics. The same hostcalls can serve writer slots, discovery tables, or any future fixed-size shared table.

**Alternative considered:** A `register_reader` / `update_reader_position` hostcall pair.
- Rejected as domain-specific leakage into the memory management layer.

### 3. Slot ownership tracked in runtime SlotManager, not in shared memory

**Decision:** The runtime maintains a `SlotManager` keyed by `(region_id, table_offset)`. Each entry tracks `slot_index → owner_process_id` and a freelist. Shared memory only contains slot *values* (visible to writers for backpressure). Slot *ownership* is never in shared memory.

**Rationale:** If ownership were in shared memory, a malicious writer (or another compromised consumer) could manipulate it. Keeping ownership in the runtime's private memory makes it untouchable by guests.

**Alternative considered:** Store ownership in a reserved field within each slot (e.g., a `(process_id, position)` tuple).
- Rejected because an attacker with write access to the page could overwrite it.

### 4. ChannelHeaderLayout as single source of truth for offsets

**Decision:** A `#[repr(C)]` struct `ChannelHeaderLayout` in `selium-guest` mirrors the shared memory page 0 layout. All offset constants are derived via `offset_of!` from the `memoffset` crate. Explicit padding fields (e.g., `_pad1: [u8; 7]`) are used where alignment demands it. Two new header fields (`layout_version: u32`, `layout_size: u32`) sit at the front of the struct.

**Rationale:** Hand-written offset constants are fragile — adding a field requires manually recomputing every constant. `memoffset` shifts them automatically. Explicit padding documents intent and prevents accidental field insertion in alignment gaps.

**Alternative considered:** A build script that generates constants from a schema file.
- Rejected as more ceremony than `memoffset` for the same result.

### 5. Layout version validation at attach time

**Decision:** At `Channel::attach`, the guest reads `layout_version` and `layout_size` from the region header and validates both match compile-time constants. Mismatch produces `Error::IncompatibleLayout`.

**Rationale:** As the layout evolves, old guests attaching to new regions (or vice versa) must fail with a clear error rather than silently misinterpreting bytes. `size_of` catches the case where a field type changes but someone forgets to bump the version.

**Alternative considered:** A registered schema registry.
- Rejected as overkill — two integers at the head of the header suffice until the layout diverges in incompatible ways across multiple versions.

### 6. HostCaller carries consumer_id for ownership validation

**Decision:** wasmtiny's `HostCaller` gains a `consumer_id: u64` field set by the runtime at guest instantiation. Slot hostcall handlers read this to validate that the caller owns the slot being written.

**Rationale:** Without caller identity in the host function context, there's no way to enforce per-slot ownership. The alternative — encoding ownership in the guest and trusting it — defeats the security boundary.

### 7. GC on process death, not on heartbeat

**Decision:** When a guest process terminates, the runtime calls `SlotManager::release_all(process_id)` which iterates all tables and frees every slot owned by that process. No heartbeat-based stale detection in day 1.

**Rationale:** The runtime already knows when a process dies (`Instance::drop` detaches all regions). Piggybacking slot release on that teardown path is zero-overhead and deterministic. Heartbeats add complexity without a clear day 1 need.

**Alternative considered:** Heartbeat-based timeout on slot updates.
- Deferred — can be layered on top if zombie slots become a problem.

### 8. Explicit methods on SharedRegion, no WriteMode dispatch

**Decision:** `SharedRegion` exposes `alloc_table_slot`, `write_table_slot`, and `free_table_slot` methods that always invoke hostcalls. There is no `WriteMode` enum or direct/hostcall fallback logic. The method IS the mechanism.

**Rationale:** The guest I/O layer (`Reader`, `BlockingReader`) doesn't need to know whether writes are direct or hostcall-mediated. It just calls the method. For consumer guests (read-only mapping), hostcall is the only option. For producer guests (read-write mapping), they call `region.mapping().write()` directly and never touch the slot methods.

**Alternative considered:** A `WriteMode` enum with Direct/Relay dispatch.
- Rejected as unnecessary complexity — the caller already knows which path it needs and uses different APIs.

## Risks / Trade-offs

- **[Hostcall overhead on position updates]** Every consumer position update crosses the host boundary. At 10k msg/s, this is ~1% overhead in Wasmtime-class runtimes. For high-throughput consumers, batching updates (e.g., every N frames) mitigates this. The writer's `minimum_reader_position()` is already approximate, so slightly stale positions only affect capacity efficiency, not correctness.
- **[Slot freelist exhaustion]** With 128 reader slots per region, a region can support at most 128 concurrent blocking readers. The runtime must return an error on `alloc_slot` when the freelist is empty.
- **[Layout version bump is breaking]** The 8-byte header insertion (version + size) makes all existing regions incompatible. This is intentional — old code can't safely interpret new regions and vice versa. The version field makes future changes less painful.
- **[Writers remain trusted]** This change does not protect against a malicious publisher. A compromised publisher with read-write access can still corrupt the stream. That's a separate, harder problem (e.g., requiring publishers to also go through hostcalls for writes, which would be prohibitively slow).

## Open Questions

1. Should we expose `consumer_id` as a public API on `HostCaller`, or keep it crate-internal? The slot hostcalls in `selium-runtime` need it, but other host functions likely don't.
2. What's the hostcall overhead in practice under the wasmtiny LLVM JIT path? The interpreter path may be slower.
3. Should the `reader_slot_counter` field in shared memory be removed now that allocation goes through the runtime's freelist? It's no longer needed for consumers, but producers might still use it for writer slot allocation.
4. Is 128 the right maximum for reader slots? If a single publisher has thousands of subscribers, they'd need to be non-blocking readers (which don't use slots).

## Migration Plan

1. Add `layout_version` and `layout_size` to `ChannelHeaderLayout`, bump version to 1, add `memoffset` dependency.
2. Implement `SlotManager` in `selium-runtime`.
3. Add `consumer_id` to wasmtiny's `HostCaller` and `Instance`.
4. Add slot hostcall variants to `selium-abi` and implement dispatch in `selium-runtime`.
5. Update `SharedRegion` with `alloc_table_slot` / `write_table_slot` / `free_table_slot`.
6. Update `Reader` and `BlockingReader` to use the new slot methods.
7. Update `Channel::attach` to validate layout version and size.
8. Update consumer attach paths to use `RegionProt::ReadOnly` with `reader_slot: None`.

**Rollback:** Each step is independently testable. The old layout and direct-write path can coexist with the new hostcall path during migration — old code uses the old mechanism, new code uses the new one. Regions created by new code are incompatible with old code (version mismatch), which is the desired behavior.
