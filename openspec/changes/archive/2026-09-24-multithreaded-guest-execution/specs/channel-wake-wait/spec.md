# Spec Delta

## MODIFIED Requirements

### Requirement: Cross-Thread Wake Delivery

A guest task parked on a host-writable ring or a host queue SHALL be woken to completion by the thread that observes the wake condition, without requiring any other thread — including the thread that bootstrapped the guest or an embedder's service loop — to pump wake delivery. For a multithreaded guest, the waking thread SHALL deliver the wake by bumping the parked task's parking word (the host-observable mirror of the task's wake counter) and notifying the guest's shared wake word; parked workers resume in place, and the woken task SHALL be resumed by whichever worker claims it from the guest's shared run queue — task capsules MAY migrate between workers, and no task-to-worker binding exists. At most one worker executes a given task's future at a time, while distinct tasks MAY execute concurrently on distinct workers. A wake that arrives while a worker is polling the task SHALL NOT be lost: the polling worker SHALL re-check pending wake state before parking, or the notifier SHALL deliver the wake when the task re-parks.

#### Scenario: Poller thread delivers an end-to-end wake

- **WHEN** the kernel network poller advances a ring generation for a region on which a guest task is registered
- **THEN** the polling thread SHALL bump the task's parking word and notify the guest's shared wake word, and the task SHALL observe its data on the worker that claims it from the run queue — with no `drain`/pump call from embedder code

#### Scenario: Wake racing an in-flight poll is not lost

- **WHEN** a wake condition is observed on thread B while a worker is polling the task
- **THEN** the polling worker SHALL re-check pending wake state before parking, or thread B's notify SHALL take effect when the task next parks, so the wake SHALL NOT be lost

#### Scenario: No embedder cooperation required

- **WHEN** an embedder runs guests without calling any wake-delivery or pumping API
- **THEN** parked tasks SHALL still progress when kernel-side events (socket data, accepted connections, EOF generation bumps) occur
