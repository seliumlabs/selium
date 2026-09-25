## ADDED Requirements

### Requirement: Cross-Process Wait Registration
A guest task parking on a host-writable shared ring SHALL be registered
with the host via a `WaitRegister { region_id, generation }` hostcall
carrying the parked `task_id` in the hostcall envelope. The runtime SHALL
record the registration and, on any host-side generation advance for
that region past the registered generation, enqueue a mailbox wake for
the registered task.

#### Scenario: Guest reader wakes on host-written data
- **WHEN** a guest task parks on an inbound network ring and the kernel
  proxy subsequently writes a frame and advances the generation
- **THEN** the runtime SHALL mailbox-wake the parked task without any
  guest-side polling or timeout fallback

#### Scenario: Registration for unattached region
- **WHEN** a guest issues `WaitRegister` for a region it has not attached
- **THEN** the runtime SHALL fail the hostcall loudly

### Requirement: Event-Driven Proxy Threads
Kernel network proxy paths SHALL NOT use sleep-based polling. Inbound
socket readiness SHALL be delivered by an OS event port
(epoll/kqueue/IOCP via mio). Outbound ring draining SHALL block on the
host wait registry (`atomic_wait32` condvar path) with a bounded timeout
backstop, and the runtime SHALL notify registered regions on every
guest→host transition (hostcall create, hostcall poll, reactor stall).

#### Scenario: No spin loops
- **WHEN** a network connection is idle
- **THEN** its proxy threads SHALL be blocked in the OS poller or the
  condvar wait, consuming no polling CPU

#### Scenario: Stall kicks outbound drain
- **WHEN** a guest writes outbound frames and then stalls its reactor
- **THEN** the runtime SHALL notify the region's host waiters, and the
  outbound pump SHALL drain the ring to the socket without waiting for
  the backstop timeout
