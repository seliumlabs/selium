# Proposal: Channel Wake/Wait for Guest I/O

## Why

Every blocking wait on channel data in the current I/O stack resolves to
one of two broken outcomes:

- **Lost wakeups**: `Reader`/`BlockingReader`/`Writer::poll_*` return
  `Poll::Pending` without registering any waker
  (`shm/src/channels/reader.rs`, `writer.rs`), so a task awaiting channel
  data parks forever — no hostcall is outstanding, no mailbox wake arrives.
- **Busy-spinning**: where lost wakeups were noticed, the workaround is
  `cx.waker().wake_by_ref()` + `Poll::Pending` hot loops
  (`wire/src/pubsub.rs`, `wire/src/rpc.rs`, guest UDP/relay loops,
  discovery `feed_loop`), which pin the guest reactor at 100% CPU and keep
  `poll_reactor` from ever stalling. The kernel proxy threads likewise poll
  generation counters at 1 ms.

The intended mechanism — `MappingBackend::atomic_wait32`/`atomic_notify`
(futex-style wait on the ring generation counter) — is a silent no-op in
every backend. `Sleep` hostcalls have no wake path either (a guest `Timer`
never fires).

This is the single largest design gap between the spine and a viable
messaging platform: a messaging-first system whose only wait strategy is
"spin or deadlock" cannot ship. It also blocks honest backpressure (Park
semantics currently mean "spin", not "park").

## What Changes

- **Real futex primitives**: implement `atomic_wait32`/`atomic_notify` for
  the guest (`PointerBackend`) using wasmtiny's `memory.atomic.wait32`/
  `memory.atomic.notify` instructions where the engine supports them, and
  for the host-side backends with a condition-variable equivalent. No
  silent-success stubs remain.
- **Waker registration in channel readers/writers**: `poll_read`/
  `poll_write` register interest in the ring generation counter; a
  generation bump wakes the task through the guest reactor instead of
  spinning.
- **Guest reactor integration**: blocked-on-channel tasks park without
  burning reactor cycles; `poll_reactor` can stall and return to the host
  when only channel waits remain.
- **Sleep wakeups**: the runtime arms a host-side timer for `SleepWait`
  operations and enqueues a mailbox wake at the deadline.
- **Spin removal**: delete every `wake_by_ref()`-spin in pub/sub, RPC,
  UDP, and guest relay loops; document the wait idiom in the guest SDK.
- **Backpressure honesty**: Park means park (wait on generation/notify);
  Drop keeps its drop-on-full semantics without parking.

### Explicitly out of scope

- Cross-host wait/notify (network bridge territory).
- Fair queuing or priority among waiters.
- `atomic.wait64` (u32 generation halves are sufficient at current scale).
- Rewriting the reactor; it gains a wake source, not a new scheduler.

## Capabilities

### New Capabilities

- `channel-wake-wait`: futex-backed wait/notify across the shared-memory
  substrate, wired into channel readers/writers and the guest reactor.

### Modified Capabilities

- `selium-shm`: readers/writers SHALL register for generation wakes
  instead of returning unwakeable `Pending` or spinning.
- `selium-guest`: the reactor SHALL park channel waits and resume on
  generation bumps; `Timer` SHALL complete at its deadline via host wake.
- `selium-kernel`: proxy readers SHALL use notify-driven waits instead of
  fixed-interval polling where the substrate allows.

## Impact

- `crates/core/memory`: real `atomic_wait32`/`atomic_notify` on
  `PointerBackend`; remove no-op stubs.
- `crates/core/shm`: reader/writer wake registration; delete spin loops.
- `crates/core/wire`: `Subscriber`/`RpcClient`/`RpcConnection` wait via
  generation instead of yield-spin.
- `crates/core/guest`: reactor wake source for generation counters;
  `Timer` completion via host timer wakes.
- `crates/core/runtime`: host-side timer wheel for `SleepWait`; mailbox
  wake on deadline.
- `crates/core/kernel`: proxy thread wait strategy.
- wasmtiny: confirm/complete `memory.atomic.wait32`/`notify` support
  (instance.rs already has `wait32` scaffolding).
