# Proposal: Event-Driven Network Proxies

## Why

The kernel's network proxies spin: outbound and UDP-send pump threads
poll the ring generation counter every 1 ms
(`PROXY_POLL_INTERVAL_MS`), inbound reads use nonblocking sockets with
1 ms sleep-retry, and full-ring backpressure is a 10 ms sleep loop. That
is a latency floor and a CPU tax on every connection, and it is the last
spin-based wait strategy left after `channel-wake-wait` landed.

Two gaps remain after that change:

1. **The host doesn't know what the guest is waiting on.** Guest ring
   waits live in the guest reactor's `GEN_WAIT_MAP`; a host-side proxy
   that bumps a ring generation has no way to know a guest task parked
   on it, so no mailbox wake is enqueued.
2. **The proxies themselves spin** instead of blocking on OS events or
   the host condvar registry that `channel-wake-wait` introduced
   (`waiters::notify`/`atomic_wait32`).

## What Changes

- **`WaitRegister` hostcall (new ABI request)**: when the guest reactor
  parks a task on a host-writable ring, it issues `WaitRegister
  { region_id, generation }` with the parked `task_id` in the envelope
  (the field already exists). The runtime records
  `(process, task, region, generation)`; any host-side generation
  advance on that region triggers `wake_process_task` → mailbox →
  guest re-poll. This is control-plane traffic (one hostcall per
  *park*), consistent with "control flows over hostcalls"; the per-op
  data-plane rejection does not apply.
- **Inbound (socket → ring)**: a single OS-event poller thread in the
  kernel (mio: epoll/kqueue/IOCP). Sockets register by `shared_id`; a
  readable event pumps available bytes into the inbound ring, bumps the
  generation, and bridges to the guest wake path above.
- **Outbound (ring → socket)**: proxy threads block on the host condvar
  registry keyed by the ring generation word instead of polling. The
  runtime *kicks* registered regions on every guest→host transition
  (hostcall create/poll, reactor stall) — a guest that writes a response
  and then awaits the next request stalls, and the stall is the wake. A
  bounded wait timeout remains as an honesty backstop.
- **Deletion of spin loops**: no `thread::sleep` retry loops remain in
  the network proxy paths.

## Capabilities

### New Capabilities

(None.)

### Modified Capabilities

- `channel-wake-wait`: cross-process wait registration and event-driven
  proxy requirements
- `selium-abi`: new `HostcallRequest::WaitRegister` variant

`WaitRegister` is unprivileged: it can only wake the registering
process's own tasks.

## Impact

- `selium-abi`: new `HostcallRequest::WaitRegister` variant.
- `selium-runtime`: wait registry, transition-kick integration in the
  hostcall path and `poll_guest_until_stalled`.
- `selium-kernel`: mio poller for inbound; condvar waits for outbound;
  spin loops deleted.
- `selium-guest`: reactor issues `WaitRegister` when parking on
  host-writable regions.
- Specs: MODIFIED `channel-wake-wait`, MODIFIED `selium-abi`, MODIFIED
  `selium-kernel`.
- Follow-on: `shared-page-fastpath` removes the transition kick on
  platforms with wait-word primitives; this change is the portable
  baseline and must remain correct without it.
