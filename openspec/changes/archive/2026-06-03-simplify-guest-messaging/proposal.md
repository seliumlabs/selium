## Why

The current `selium-guest` IO layer bakes host-visible metadata (magic, capacity, writer counts, reader slots) into every shared memory region and uses a separate `Signal` primitive for cross-process notification. This couples the host to guest-level messaging semantics and introduces unnecessary hostcalls. Switching to a "dumb host, smart guest" model where the host provides only memory pages and the guest builds everything on top via native WASM atomics eliminates the signal hostcall path entirely, enables kernel-enforced per-page memory protection for multi-tenant isolation, and makes the guest library the sole owner of messaging protocol logic.

## What Changes

- **BREAKING**: Remove `Signal` type and all signal hostcalls (`signal_create`, `signal_wait`, `signal_notify`) from the ABI — guests use native `memory.atomic.wait32`/`notify` on mapped shared pages instead
- **BREAKING**: Remove `SharedMemory` hostcall-based read/write — shared regions are mapped directly into guest linear memory via wasmtiny's `alloc_region`/`free_region`/`attach_region` host functions
- **BREAKING**: Strip all host-visible metadata from shared region layout — only ring buffer data and a generation counter live in the shared region; writer IDs, reader slots, and cursor state move to per-guest private memory
- Replace two-phase frame write (write header, write payload, rewrite header) with single-phase write using release/acquire fencing — one header write instead of two, no abort frames needed
- Collapse three-layer error hierarchy (`io::Error` → `channels::Error` → `RpcError`) into a single flat `Error` enum
- Extract `RpcClient`/`RpcConnection` into a separate `selium-rpc` crate and `LiveTable` into `selium-tables`
- Add per-page `mprotect` support: each reader cursor gets its own page so untrusted consumers can be mapped `PROT_READ` on data pages with a single writable cursor page
- Add `RegionProt` parameter to `alloc_region` and `attach_region` for read-only vs. read-write guest mappings
- Replace fixed-iteration CAS spin loops with exponential backoff

## Capabilities

### New Capabilities
- `guest-shared-memory`: Three-function host ABI (`alloc_region`, `free_region`, `attach_region`) that maps shared pages into guest linear memory, replacing the old `SharedMemory` + `Signal` hostcall surface
- `per-page-memory-protection`: Per-page `mprotect` enforcement where consumer instances map data pages `PROT_READ` with a single writable cursor page, preventing malicious guests from corrupting shared buffers

### Modified Capabilities
- `selium-guest`: Simplified IO module — flat error type, single-phase write protocol, host-agnostic shared region layout, exponential backoff in CAS loops, extracted RPC and LiveTable crates
- `selium-abi`: Removed `Signal` variants from `HostcallRequest`/`HostcallOutput`; removed `SharedMemory` read/write hostcalls; added `AllocRegion`, `FreeRegion`, `AttachRegion` hostcall variants with `RegionProt` parameter
- `selium-runtime`: Replace signal and shared-memory hostcall dispatch with passthrough to wasmtiny's new mmap-based shared memory API

## Impact

| Area | Impact |
|------|--------|
| `selium-guest` | Major simplification: `region.rs` loses ~300 lines of host-visible layout; `error.rs` collapses to single enum; `ring_buf.rs` switches to single-phase write; `pubsub.rs` and `rpc/` extracted to separate crates |
| `selium-abi` | `HostcallRequest` and `HostcallOutput` lose all `Signal*` and `SharedMemory*` variants; gain `AllocRegion`, `FreeRegion`, `AttachRegion` |
| `selium-runtime` | Hostcall dispatch table replaces signal/shared-memory handlers with wasmtiny region calls |
| `selium-rpc` (new) | New crate extracted from `selium-guest/src/io/rpc/` |
| `selium-tables` (new) | New crate extracted from `selium-guest/src/io/tables.rs` |
| `selium-kernel` | Signal proxying code removed; shared region management delegated to wasmtiny |
| Guest WASM modules | Must update to new ABI; `Signal::wait` calls become `memory.atomic.wait32` on the generation counter page |
