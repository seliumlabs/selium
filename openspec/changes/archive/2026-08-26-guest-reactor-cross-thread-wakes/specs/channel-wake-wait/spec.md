## ADDED Requirements

### Requirement: Cross-Thread Wake Delivery
A guest task parked on a host-writable ring or a host queue SHALL be
woken to completion by the thread that observes the wake condition:
the waking thread enqueues the mailbox wake and executes the guest
reactor under the runtime's execution guard, without requiring any
other thread — including the thread that bootstrapped the guest or an
embedder's service loop — to pump wake delivery. Execution of a
guest's reactor remains single-threaded-at-a-time via the execution
guard; wakes that arrive while the guard is held SHALL be delivered
via post-release re-check of pending mailbox state and SHALL NOT be
lost.

#### Scenario: Poller thread delivers an end-to-end wake
- **WHEN** the kernel network poller advances a ring generation for a
  region on which a guest task is registered, while no other thread is
  executing that guest
- **THEN** the polling thread SHALL enqueue the mailbox wake, execute
  the reactor, and the parked task SHALL observe its data — with no
  `drain`/pump call from embedder code

#### Scenario: Wake racing an in-flight poll is not lost
- **WHEN** a wake condition is observed on thread B while thread A is
  executing the same guest's reactor under the execution guard
- **THEN** thread A SHALL re-check pending mailbox state after
  releasing the guard and deliver the wake, or thread B SHALL acquire
  the freed guard and deliver it

#### Scenario: No embedder cooperation required
- **WHEN** an embedder runs guests without calling any wake-delivery
  or pumping API
- **THEN** parked tasks SHALL still progress when kernel-side events
  (socket data, accepted connections, EOF generation bumps) occur
