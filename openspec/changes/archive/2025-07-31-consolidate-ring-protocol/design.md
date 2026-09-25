# Design: Consolidate the Ring Protocol

## Context

The ring protocol has two execution environments with different atomicity
domains: guests operate on mapped memory with hardware atomics
(`PointerBackend`), the kernel operates through wasmtiny's Store mutex
(`KernelBackend`). Any shared implementation must be parameterised over
the backend's primitive ops (`MappingBackend` already exists:
read/write/atomic u64 ops) rather than over concrete memory types.

## Goals / Non-Goals

**Goals:**

- One definition of offsets, frame codec, reservation, and slots.
- Kernel uses it through its Store-mediated backend; guests use it
  through `PointerBackend`. Identical behaviour in both.
- Zero layout changes (wire compatible).

**Non-Goals:**

- Cross-domain atomicity redesign, slot recycling, the bridge itself.

## Decisions

1. **`selium-shm::layout` (new module) holds the protocol**: offset
   constants, `FrameHeader` (moved from `selium-wire::frame` — it is a
   ring concept, not a transport concept), `reserve_tail_next`, slot
   encode/decode, and `RingReader`/`RingWriter` primitives generic over
   `MappingBackend`. `ChannelRegion` becomes layout + provider plumbing.
   (Moving `FrameHeader` re-exports through `selium-wire` to avoid API
   breakage.)
2. **The kernel stops reimplementing**: `network_runtime.rs` proxies and
   `drain_log_channel` call the shared `RingReader`/`RingWriter` over
   `KernelBackend` (already exists via `RuntimeRegionProvider`; move or
   share it so the kernel can use it without depending on the runtime).
3. **Multi-memory header moves to `selium-memory`**: `MultiMemoryHeader
   { write, parse }` + `SHARED_REGION_MAGIC` (already there); shm/rpc.rs
   and the kernel use it. Kernel's stream-region constructor uses the
   same writer.
4. **Atomicity rule, documented + asserted**: each ring declares its
   writer domain at creation (`ResourceKind` is informational today; add a
   debug assertion in shared reservation paths where a domain tag is
   available). Mixed-domain writes are out-of-contract, not supported.
5. **Kernel reader slots come from the shared counter** (no hard-coded
   slot 0): `RingReader::open` allocates through `reader_slot_counter`
   like any other reader.

## Risks / Trade-offs

- **Backend trait friction**: `MappingBackend` covers everything the
  layout needs, but the kernel's mutex-mediated atomics make
  `compare_exchange_u64` semantics subtly different under contention
  (documented constraint, not a code fix).
- **Test migration**: existing kernel network tests were written against
  the bespoke helpers; they port to the shared primitives with the same
  assertions (echo-proxy tests preserved).
- **Header parse is fallible**: consolidating means the kernel now
  validates the header the same way guests do — a small behaviour change
  for malformed regions (fails fast instead of misreading).
