## Context

The current `selium-guest/src/io/` module provides a lock-free shared-memory ring buffer with channel semantics, pub/sub, RPC, and live tables. It relies on two host-provided primitives: `SharedMemory` (byte-level read/write through hostcalls) and `Signal` (cross-process notification via hostcalls). The shared memory region layout bakes host-visible metadata (magic, capacity, writer/reader counts, reader slots) into fixed header offsets.

This design couples the host to guest-level messaging semantics. Every notification requires a hostcall. Every shared memory access copies through host buffers. The host must understand ring buffer cursor layout to manage reader slots.

The "dumb host, smart guest" model inverts this: the host provides only memory pages mapped into guest linear memory, and the guest builds everything on top using native WASM load/store and atomic instructions. This eliminates the signal hostcall entirely (`memory.atomic.wait32`/`notify` serve the same purpose) and moves all protocol logic into the guest library.

## Goals / Non-Goals

**Goals:**
- Reduce the host ABI to 3 functions: `alloc_region`, `free_region`, `attach_region`
- Remove all host-visible metadata from shared region layout
- Replace two-phase frame write with single-phase write using release/acquire fencing
- Collapse error hierarchy to a single flat enum
- Extract RPC and LiveTable into separate crates
- Add per-page `mprotect` for multi-tenant consumer isolation
- Maintain lock-free progress guarantees with exponential backoff

**Non-Goals:**
- Changing the ring buffer cursor model (strong/weak reader/writer semantics are preserved)
- Cross-host RDMA or distributed shared memory — this is local-only
- Removing the pub/sub or channel abstractions — only the implementation changes
- WASM multi-memory — shared regions are extended into a single linear memory

## Decisions

### 1. Shared region layout: data-only, no host-visible metadata

The current `ChannelRegion` header (4096 bytes) contains magic, capacity, writer_count, reader_count, next_tail, tail_cache, signal_shared_id, next_writer_id, next_mutation_id, and 128 reader slots. All of this moves into per-guest private memory.

The shared region becomes:

```
┌─────────────────────────────────────┐
│ Page 0: generation_counter (u64)    │  ← for memory.atomic.wait32/notify
│         7 bytes padding            │
├─────────────────────────────────────┤
│ Page 1..N: ring buffer data        │
└─────────────────────────────────────┘
```

The generation counter is incremented by writers after committing data. Readers `wait32` on it to block until new data arrives. Everything else — tail cursor, writer IDs, reader slots, channel configuration — lives in the guest's private linear memory.

**Rationale:** The host has no business knowing ring buffer internals. This also means a guest can change its cursor management strategy without a host update. The generation counter is the minimum shared state needed for cross-process notification via native atomics.

**Alternatives considered:**
- Zero shared metadata (IPC via side-channel only) — rejected because the generation counter enables `wait32`/`notify` without any hostcall
- Keep signal as separate hostcall — rejected because it requires a host round-trip for every notification

### 2. Single-phase write with release/acquire fencing

Current two-phase write:
```
1. Write header with READY=0
2. Write payload
3. Rewrite header with READY=1
4. Fire signal
```

New single-phase write:
```
Writer:                         Reader:
write payload at slot+16        loop:
fence (release)                    fence (acquire)
write header (len | READY)         load header
notify(generation_counter)         if READY: read payload
                                   else: wait32 on generation_counter
```

The release fence ensures the payload is visible in memory before the READY flag. The acquire fence ensures the READY flag load orders before the payload read. On x86 and ARM, these fences are zero-cost (x86 has TSO; ARM `stlr`/`ldar` are the same cost as regular stores/loads).

**Rationale:** One header write instead of two. No abort frames needed — if a writer crashes mid-payload, the header is never written and the reader times out. The WASM threads proposal guarantees that `memory.atomic.wait32` / `memory.atomic.notify` have the correct acquire/release semantics on the underlying futex.

**Alternatives considered:**
- Keep two-phase write with abort frames — simpler migration but leaves complexity in the hot path
- Use seqlock approach — rejected because seqlocks require retry loops on the reader side and don't work well with `wait32` blocking

### 3. Per-page reader cursor isolation

Each reader cursor gets its own page. When a consumer attaches, it maps the entire region `PROT_READ` except for its dedicated cursor page which is `PROT_READ | PROT_WRITE`:

```
┌─────────────────────────────────────┐
│ Page 0: generation_counter          │  PROT_READ for consumers
├─────────────────────────────────────┤
│ Page 1: reader slot 0               │  RW for reader 0 only
├─────────────────────────────────────┤
│ Page 2: reader slot 1               │  RW for reader 1 only
├─────────────────────────────────────┤
│ Page 3..N: ring buffer data         │  PROT_READ for consumers
└─────────────────────────────────────┘
```

The host enforces this via `mprotect`. A malicious consumer attempting to write to data pages or another reader's cursor gets `SIGSEGV` → trap.

**Rationale:** Multi-tenant deployments where untrusted consumers read from a trusted producer's channel need kernel-enforced isolation. At 4KB per reader slot, 128 slots cost 512KB — acceptable overhead.

**Alternatives considered:**
- Application-level enforcement only — simpler but doesn't protect against malicious guests
- `userfaultfd`-based write trapping — higher overhead per write, more complex host code
- Write-only mappings — x86 cannot enforce write-without-read at the page table level; requires PKU or emulation

### 4. `alloc_region` returns page offset, not a handle

The host ABI is:

```rust
fn alloc_region(pages: u32, prot: RegionProt) -> (region_id: u64, page_offset: u32);
fn free_region(region_id: u64);
fn attach_region(region_id: u64, reader_slot: Option<u32>, prot: RegionProt) -> page_offset;
```

`page_offset` is the index into the guest's linear memory (in pages) where the region is now visible. The guest accesses it with regular `i32.load` / `i32.store` / `memory.atomic.*` at that offset.

**Rationale:** No handles to pass to every load/store. The offset is a constant the guest caches. This is the minimal possible interface.

### 5. Exponential backoff in CAS loops

Replace hardcoded 1024-iteration spin loops with:

```rust
fn reserve_tail(&self, len: u64) -> Option<u64> {
    let mut delay = 1;
    loop {
        match self.try_reserve_tail(len) {
            Ok(pos) => return Some(pos),
            Err(Contention) => {
                for _ in 0..delay { core::hint::spin_loop(); }
                delay = (delay * 2).min(64);
            }
            Err(Full) => return None,
        }
    }
}
```

No arbitrary iteration cap. Under contention, writers back off exponentially to max 64 spin-loop iterations between attempts. Under no contention, the first attempt succeeds (delay is only used on CAS failure).

### 6. Flat error enum

Replace the three-layer hierarchy with:
```rust
enum Error {
    BufferFull,
    BufferEmpty,
    ReaderBehind { pos: u64, tail: u64 },
    ReservationContended,
    InvalidFrame,
    ChannelClosed,
    ConnectionLost,
    SerializationFailed,
}
```

No `From` chains, no nested wrapping. Each variant maps directly to a distinct failure mode.

### 7. RPC and LiveTable extraction

`rpc/` and `tables.rs` move to `selium-rpc` and `selium-tables` crates respectively. The core `selium-guest` IO module exports only `RingBuf`, `Channel` (with strong/weak reader/writer), and `pubsub`. These are the messaging primitives. RPC and LiveTable are messaging patterns built on top.

## Risks / Trade-offs

| Risk | Mitigation |
|------|------------|
| Breaking ABI change for all existing guest modules | Version the ABI; old guests continue to work with old `Signal`+`SharedMemory` hostcalls behind a compatibility layer |
| Generation counter overflow on 32-bit `wait32` | Use `wait64` on 64-bit counter for 64-bit guests; 32-bit guests wrap safely after ~4 billion notifications (decades at 100K/s) |
| `mprotect` per-page overhead at attach time | One `mprotect` syscall per reader slot per attach; amortized over the lifetime of the channel (minutes to hours) |
| Release/acquire fence portability | WASM threads spec guarantees correct lowering to all target architectures; x86 fences are no-ops; ARM uses `stlr`/`ldar` |
| Snapshot compatibility with shared pages | Snapshots skip shared pages (they're not owned by the instance); restoring an instance re-attaches shared regions by ID |
