# Proposal: Harden Consumer Shared Memory Against Malicious Writes

## Why

Channel consumers (`Subscriber`, `BlockingReader`, `Reader`) share raw `*mut u8` memory mappings with publishers. A compromised consumer can corrupt the publisher's stream by writing to arbitrary shared memory — overwriting frame data, bumping the generation counter, or moving `next_tail`. It is reasonable for users to assume consumers have read-only access, making this a serious foot gun. The existing per-page `reader_slot` mechanism provides hardware-level protection at page granularity, but the current layout places reader slots on the same page as other mutable metadata (`next_tail`, `writer_count`, `generation_counter`), and even with a dedicated page, any consumer sharing that page can corrupt another consumer's position.

## What Changes

- **Consumers get `PROT_READ` only**: Consumer guests attach shared regions with `RegionProt::ReadOnly` and no writable `reader_slot`. All shared memory writes trap.
- **Hostcall-mediated slot writes**: Three new generic hostcalls (`alloc_slot`, `write_slot`, `free_slot`) allow consumers to update their reader position through validated runtime-mediated operations. The runtime enforces per-slot ownership.
- **Slot ownership tracked in the runtime**: A `SlotManager` in `selium-runtime` tracks which process owns which table slot. Slots are garbage-collected on process death.
- **`ChannelHeaderLayout` as layout source of truth**: A `#[repr(C)]` struct replaces hand-written offset constants. All offsets are derived via `memoffset::offset_of!`, with layout versioning and attach-time validation.
- **wasmtiny `HostCaller` carries caller identity**: A `consumer_id: u64` field on `HostCaller` enables the slot hostcalls to validate that the calling guest owns the slot it's trying to write.

## Capabilities

- **Slot table authority**: A new capability controls whether a guest can call `alloc_slot` / `write_slot` / `free_slot` on a given region. Consumer guests receive this capability scoped to their region.
- **Shared memory authority**: Unchanged — consumer guests still need `attach_region` authority to map shared memory.

## Impact

### Modified Crates

- `selium-abi` — new hostcall variants
- `selium-guest` — `ChannelHeaderLayout`, `memoffset`, layout validation, `SharedRegion` slot methods
- `selium-runtime` — `SlotManager`, hostcall dispatch, consumer attach enforcement
- `wasmtiny` — `HostCaller::consumer_id`, `Instance::consumer_id`

### Specs Modified

- `guest-shared-memory` — new layout, hostcall slot writes, versioning
- `selium-abi` — slot hostcalls
- `selium-runtime` — `SlotManager`, consumer lifecycle
- `selium-guest` — layout struct, validation, `SharedRegion` methods
- `framed-io` — reader slot ops via hostcall
- `per-page-memory-protection` — consumer default changes
- `wasmtiny` (new) — `HostCaller` identity

## Deferred

- Generalising `SharedTable<T>` as a guest-facing generic — the arithmetic is simple enough for now
- Writer-slot hostcall path — publishers still write directly; only consumers are restricted in this change
- Heartbeat-based stale slot detection — process death is the primary GC trigger; heartbeats can be layered on later
