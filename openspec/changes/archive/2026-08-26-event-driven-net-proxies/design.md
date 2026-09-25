# Design: Event-Driven Network Proxies

## Context

`channel-wake-wait` landed in-guest generation waits, the host condvar
registry (`waiters::`), and the mailbox wake path
(`wake_process_task` → `poll_guest_until_stalled`). The network proxies
predate it and still spin. See proposal.md for the two remaining gaps.

## Goals / Non-Goals

**Goals:**

- Zero sleep-based polling in network proxy paths
- Guest tasks parked on host-writable rings wake via the mailbox

**Non-Goals:**

- Per-OS wait-word fast paths (see `shared-page-fastpath`)
- Cross-host wake propagation (bridge/multi-host territory)
- Rewriting the guest reactor (it gains a registration call, not a new
  scheduler)

## The wake graph being closed

```
GUEST                                        HOST
─────                                        ────
reader polls inbound ring → Pending
reactor: WaitRegister{region, gen, task} ──▶ runtime wait registry
reactor stalls ────────────────────────────▶ (guest asleep)

                                             mio poller: socket readable
                                             pump bytes → inbound ring
                                             bump generation
                                             registry match (region, gen)
                                             wake_process_task(task) ──▶ mailbox
                                                                      ──▶ poll_guest_until_stalled
reader wakes ◀────────────────────────────── drain_mailbox

writer: write outbound frames
next hostcall OR reactor stall ────────────▶ runtime kicks region
                                             (waiters::notify)
                                             outbound proxy wakes from
                                             condvar wait → drain ring
                                             → socket write
```

## Decisions

### Why a `WaitRegister` hostcall instead of shared-page tricks (for now)

The portable baseline cannot assume the guest can wake a host thread
through shared memory (in-guest `atomic_notify` is an honest no-op on
stable WASM). Every guest→host transition already funnels through the
runtime, so registration and kicks ride existing control flow. One
hostcall per park is control-plane; the rejected "per-op hostcall data
plane" was per *operation*, not per *park*.

### Why mio for inbound and condvars for outbound

- Inbound readiness is an OS property → OS event ports (epoll/kqueue/
  IOCP via mio). One poller thread replaces one blocking thread per
  connection.
- Outbound readiness is a shared-memory property → not pollable by the
  OS. The host `waiters` condvar registry (keyed by the ring generation
  word's host address) is the portable block; runtime kicks at
  guest→host transitions provide the wake. The bounded timeout backstop
  covers "guest computes for a long time after writing" — correctness
  never depends on the kick.

### Honesty requirements

- `WaitRegister` for a region the process has not attached fails loudly.
- Registry entries are per-process; a process can only wake its own
  tasks.
- Duplicate/stale registrations are harmless: a wake for a
  generation that already advanced resolves to an immediate poll.

## Risks / Trade-offs

- **Thundering re-poll**: kicks wake the whole guest (`poll_guest_until_stalled`),
  not one task. Acceptable at current scale (single-threaded reactor);
  noted for `shared-page-fastpath` follow-up.
- **Kick coverage**: every guest→host transition must kick. The hostcall
  create/poll paths and the stall path are enumerated in the tasks;
  missing one degrades to timeout latency, not deadlock — verified by
  tests.
