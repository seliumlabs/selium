## Purpose

Define the staged fast path that lets a guest's atomic notify wake host
waiters directly on host-shared memory regions — Stage 1 via a unified
per-region wait registry (portable), Stage 2 via optional per-OS
wait-word primitives — with detection and fallback honesty and no
platform becoming dependent on either stage.

## ADDED Requirements

### Requirement: Guest Notify Emission on Ring Writes
When built with the atomics feature, the guest ring write path SHALL
execute `memory.atomic.notify` on the ring's generation word after every
generation bump, and guest modules using atomic instructions SHALL
declare their memory shared with a maximum. Stable builds SHALL retain
the honest no-op notify and SHALL keep working via the kick path
unchanged.

#### Scenario: Feature-gated guest write
- **WHEN** an atomics-enabled guest writes a frame and bumps the ring
  generation
- **THEN** it SHALL emit a notify on the generation word in the same
  write path

#### Scenario: Stable guest unaffected
- **WHEN** a guest built without the atomics feature writes a frame
- **THEN** no atomic instruction SHALL be emitted and the runtime kick
  path SHALL continue to deliver the wake

### Requirement: Unified Shared-Region Wait Registry (Stage 1)
Host-side waiters on engine-backed shared regions SHALL block on the
engine's per-region wait registry through its public host-facing API, so
a guest notify wakes them without a runtime transition kick. The
runtime SHALL suppress transition kicks for regions with this path
active and SHALL keep kicking for all others.

#### Scenario: Guest write wakes outbound drainer
- **WHEN** a guest writes outbound frames and notifies on the ring
  generation word, and Stage 1 is active for that region
- **THEN** the host drainer SHALL wake directly from its wait on the
  region registry with no runtime kick involved

#### Scenario: Mixed environment
- **WHEN** one region has Stage 1 active and another does not
- **THEN** the runtime SHALL suppress kicks only for the fast-path
  region

### Requirement: Optional Per-OS Wait-Word Primitives (Stage 2)
Where a platform wait-word primitive is wired, its notify/wait race
conformance test has passed, and the build enables the engine's platform
wake emission (a build-time opt-in detected via the engine's support
flag), host waits SHALL use that primitive and the engine's notify SHALL
emit the matching platform wake. Platforms without all three SHALL use
Stage 1 with identical semantics. macOS SHALL permanently use Stage 1:
Darwin rejects the wait-word syscalls (`__ulock_wait`,
`os_sync_wait_on_address`, the restricted futex) for ordinary binaries,
so the host can never park on the word the engine would wake; its
`__ulock_*` backend is retained only for the day that changes.

#### Scenario: Platform without Stage 2
- **WHEN** the host platform has no wired wait-word primitive, its
  conformance test has not passed, or the build did not enable the
  engine's platform wake emission
- **THEN** waits SHALL use Stage 1 (or the portable path) with no
  behavioural difference beyond latency

#### Scenario: Conformance-gated enablement
- **WHEN** the Stage 2 notify/wait race conformance test fails on a
  platform
- **THEN** Stage 2 SHALL remain disabled there and Stage 1 SHALL carry
  every wait with identical semantics

### Requirement: Detection, Not Configuration
Fast-path availability SHALL be detected per region at attach time from
the engine support flag and the attaching guest's own module bytes
(shared-memory declaration plus atomic-notify opcodes — the ground truth
the engine's validator uses). There SHALL be no user-facing
configuration for this behaviour, and fallback SHALL be automatic. A
region's fast path SHALL be active only when every attaching process's
guest is capable, so a stable-built guest sharing a region with an
atomics guest retains its transition kicks.

#### Scenario: Attach detects capability
- **WHEN** a region is attached and the engine reports no fast-path
  support, or the attaching guest's module declares no shared memory or
  contains no atomic notify opcodes
- **THEN** the runtime SHALL record the region as portable-path and
  SHALL NOT attempt kick suppression for it

#### Scenario: Mixed attachers
- **WHEN** a region is attached by both a fast-path-capable guest and a
  guest whose module is not capable
- **THEN** the runtime SHALL keep transition kicks for that region until
  the incapable attacher detaches

### Requirement: No Spin Regression
Neither stage SHALL introduce sleep-based polling anywhere; platforms
without the fast path SHALL retain the spin-free portable path from
`channel-wake-wait` as extended by `event-driven-net-proxies`.

#### Scenario: Idle connection on portable path
- **WHEN** a connection is idle on a platform without the fast path
- **THEN** its proxy threads SHALL remain blocked in the OS poller or
  condvar wait with no polling CPU
