# Proposal: Consolidate the Ring Protocol

## Why

The shared-memory ring protocol — the single most important data structure
in the platform — is currently implemented **three and a half times**:

1. `selium-shm` (`region.rs`, `ring_buf.rs`, `channels/`): the canonical
   implementation, used by guests and the runtime.
2. `kernel/network_runtime.rs` (~600 lines): a hand-rolled reimplementation
   of the frame codec, `reserve_tail` CAS, reader/writer slot scans, and
   wrap-around arithmetic for the kernel's network proxies, with host-mutex
   "atomics" that are not atomic against guest hardware atomics.
3. `kernel/process.rs::drain_log_channel`: a fourth, partial
   reimplementation of frame parsing for guest log drains (its ring-geometry
   bug was only fixed on the spine branch).
4. The multi-memory region header (magic + entry table) is parsed/written
   in **four** places with the magic constant defined four times
   (`selium-memory`, `shm/rpc.rs`, `kernel/network_runtime.rs`, plus two
   copies already deleted with `guest/net/`).

Every copy is a bug farm: the kernel's log-drain mask bug shipped, the
host/guest atomicity-domain mismatch is one feature away from data
corruption, and the planned network bridge would inherit the buggiest
copy. The kernel also duplicates reader/writer slot logic with a hard-coded
slot-0 convention that collides with guest-allocated slots.

Consolidation is a prerequisite for the network bridge and for
`channel-wake-wait` (wake semantics must exist once, not thrice).

## What Changes

- **One ring implementation**: extract the offset constants, frame codec,
  reservation/CAS logic, and slot arrays into a form usable by BOTH the
  shm crate (guests, real atomics) and the kernel (host, mutex-mediated).
  The shm layout stays the single source of truth; the kernel consumes it
  rather than re-deriving it.
- **One multi-memory header**: a single definition + parser/writer in
  `selium-memory` (magic, count, entry table); all four sites use it.
- **Delete the kernel's ring reimplementation** in
  `network_runtime.rs`: proxies use the shared implementation via a
  host-side accessor (the kernel's Store-mediated backend implements the
  same primitive ops the layout expects).
- **Delete `drain_log_channel`'s bespoke parser**: log drains use the
  shared frame reader with a caller-supplied position.
- **Document the atomicity rule**: which rings are single-writer-domain
  (guest-side OR host-side, never mixed) until a real cross-domain
  protocol exists; assert it where enforceable.
- **Slot-0 collision**: the kernel allocates its reader slots through the
  same counter as guests (no hard-coded slot 0).

### Explicitly out of scope

- Changing the wire format/layout (backwards compatible: same offsets).
- The network bridge itself (lands after this on the consolidated base).
- Cross-domain atomicity redesign (documented constraint instead).
- Reader/writer slot recycling (tracked with RPC lifecycle work).

## Capabilities

### New Capabilities

- `ring-protocol-core`: single shared definition of the ring layout, frame
  codec, reservation, and slot protocol, consumable by guest and host.

### Modified Capabilities

- `selium-kernel`: network proxies and log drains SHALL use the shared
  ring implementation; no bespoke frame/slot code SHALL remain in the
  kernel.
- `selium-shm`: the layout constants and codecs SHALL be importable
  without pulling guest-only dependencies (provider-based backends).

## Impact

- `crates/core/memory`: gains the multi-memory header definition + parser.
- `crates/core/shm`: layout/codec split from provider plumbing for reuse.
- `crates/core/kernel`: deletes ~600 lines of `network_runtime.rs` ring
  code and the bespoke log drain; consumes the shared implementation.
- Unblocks: network bridge (builds on one ring), `channel-wake-wait`
  (one place for wake semantics), RPC lifecycle fixes.
