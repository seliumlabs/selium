# Design: Shared-Page Wait/Wake Fast Path

## Context

`event-driven-net-proxies` leaves one indirect hop: a guest writing to
an outbound ring cannot wake the host drainer directly, so the runtime
kicks on guest→host transitions. A source audit of wasmtiny shows most
of the machinery already exists:

- threads-proposal validation (shared memory requires a maximum; atomic
  opcodes require shared memory; per-op alignment checks)
- `memory.atomic.notify`/`wait32`/`wait64` decoded and executed
- regions backed by `shm_open` + `mmap(MAP_SHARED)` — identical physical
  pages mapped into guest linear memory and visible to host threads
- per-region waiter registries (`Arc<RwLock<HashMap<offset,
  SharedWaiter>>>`) shared across attachments

The gaps are: the wait side of that registry was `pub(crate)` (nothing
outside the engine can block on it), and nothing on the Selium guest
side ever emits a notify (the ring write path calls only the in-process
`wake_generation_waiters`; `PointerBackend::atomic_notify` is an honest
no-op on stable WASM).

## Goals / Non-Goals

**Goals:**

- Guest write wakes host waiters directly, portably (Stage 1)
- Optional per-OS wait-word latency (Stage 2)
- Portable condvar path remains the correctness baseline

**Non-Goals:**

- Requiring the fast path on any platform
- Cross-host wakes; `wait64`; fair queuing among waiters
- In-guest blocking waits (guests park via the reactor/mailbox; a
  guest-side `wait32` on the interpreter thread would stall the
  instance — the engine supports it, Selium deliberately does not use it)

## Decisions

### Stage 1: unify the waiter registry before touching OS primitives

Today there are two condvar registries for the same pages: wasmtiny's
per-region one (which guest notify pokes) and `selium-memory`'s
address-keyed one (which host proxies sleep in). Nobody bridges them —
that is the entire reason transition kicks exist. Stage 1 deletes the
second registry for wasmtiny-backed regions: host proxies block on the
engine's per-region waiters via a new public API. Portable, small, and
it captures most of the value. Alternative considered (jumping straight
to futex) rejected: more per-OS surface for a bookkeeping optimisation.

### Stage 2: OS wait-words as gated opt-in

| Platform | Wait | Wake | Status |
| --- | --- | --- | --- |
| Linux | `futex(FUTEX_WAIT)` | `futex(FUTEX_WAKE)` | enablement = conformance in CI |
| Windows | `WaitOnAddress` | `WakeByAddressAll` | wired, awaiting conformance evidence |
| FreeBSD | `_umtx_op` wait | `_umtx_op` wake | wired, awaiting conformance evidence |
| macOS | `__ulock_wait` | `__ulock_wake` | **permanently Stage 1** (see below) |

Viable because regions are `MAP_SHARED`: wait-word keys match across the
guest and host mappings of the same pages. The engine emits the platform
wake from its notify path (guests never syscall — the interpreter emits
it).

Enablement is a build-time chain: the `stage2-wait-words` cargo feature
(`selium-runtime` → `selium-kernel` → wasmtiny's
`platform-wake-emission`) compiles in the engine's emission; the host
detects it via `HostWaitSupport::RegistryAndOsWake` plus its own wired
wait-word (`stage2_active`). The feature self-gates per OS — macOS
compiles it to a no-op — so it is safe to enable anywhere, but it is off
by default and MAY only be enabled where the notify/wait race
conformance test (`stage2_notify_wait_race_conformance`, plus the
engine-side `guest_notify_wakes_host_os_waiter_conformance`) has passed;
CI runs it on Linux. Any failure falls back to Stage 1 permanently for
that platform.

macOS is excluded empirically, not speculatively: on macOS 26.x,
`__ulock_wait` returns junk errors (`-EFAULT` on static words,
`-EOWNERDEAD` on heap words) even on private memory and never parks —
Darwin only honours the private `os_sync_wait_on_address`/`__ulock_*`
family for entitled callers, and rejects the restricted futex syscall
outright. The `__ulock_*` backend (constants audited against XNU's
`bsd/sys/ulock.h`: `UL_COMPARE_AND_WAIT == 0x2`, `ULF_WAKE_ALL ==
0x100`) is retained for the day that changes; `available()` returns
false there.

### Guest-side emission is the driver

Three requirements, all feature-gated on `nightly-wasm-atomics`:

1. `selium-shm`'s generation-bump path calls `atomic_notify` on the ring
   generation word with wake count **1** (today: zero call sites). The
   count was `u32::MAX` while the notify was a stable no-op; a genuine
   notify would make the engine iterate the count under the waiter
   registry's read lock (4 billion iterations stalls every registrant's
   deregistration — found by the end-to-end test). One host drainer
   parks on the generation word, so 1 is exact.
2. Guest modules declare memory 0 **shared with a maximum** — the
   validator rejects atomic instructions otherwise.
3. Toolchain: nightly + `stdarch_wasm_atomic_wait` + `+atomics`.

Stable builds keep the current honest `Ok(0)` no-op and everything
falls back to kicks.

### Detection, not configuration

Availability is detected per region at attach from the engine support
flag plus the attaching guest's **own module bytes**, probed once at
spawn: a shared-memory declaration in the memory section AND the
`memory.atomic.notify` opcode sequence (`0xFE 0x00`) in the code
section. A shared-memory declaration alone does not prove the guest
emits notifies; a hand-linked shared module without the atomics feature
would otherwise be misclassified, its kicks suppressed, and its drainers
left to the backstop. The opcode scan is false-positive-only (a genuine
notify always produces the sequence), and combined with the
shared-memory requirement the misclassification window is a module that
declares shared memory, contains a coincidental `0xFE 0x00` immediate,
and never notifies — contrived by construction.

Eligibility is **per attachment**: a region's fast path is active only
when every attaching process's guest is capable, so a stable-built guest
sharing a region with an atomics guest keeps its kicks until it detaches.
No user-facing knob.

## Risks / Trade-offs

- Stage 1 couples `selium-memory`'s host backend to a wasmtiny API →
  the coupling already exists (path-patched dependency); the backend
  delegates only for engine-backed regions and keeps its own registry
  for heap/test providers.
- macOS `__ulock_*` is undocumented → moot: excluded permanently on
  empirical kernel grounds, not just undocumented-API caution.
- A missed wake would hang a proxy → the bounded-timeout backstop from
  `event-driven-net-proxies` stays in place even on the fast path until
  the conformance evidence is boring.
