## Purpose

`guest-worker-pool` defines how a single guest process runs its tasks concurrently on a dedicated pool of OS worker threads sharing the guest's linear memory, so one guest can use more than one CPU core.

## Requirements

### Requirement: Dedicated OS Worker Threads

A multithreaded guest SHALL be executed by a pool of dedicated OS worker threads. The pool size SHALL be bounded by the host's configuration and SHALL NOT exceed the number of available CPU cores unless explicitly overridden. Each worker SHALL enter the guest's instance over the guest's shared linear memory.

#### Scenario: Worker pool is provisioned at spawn

- **WHEN** a guest configured for multithreaded execution is spawned
- **THEN** the runtime SHALL start the configured number of dedicated worker threads, each bound to the guest instance

#### Scenario: Pool size respects the core bound

- **WHEN** the host does not explicitly override the pool size
- **THEN** a multithreaded guest's worker count SHALL default to the number of available CPU cores and SHALL NOT exceed it

### Requirement: Concurrent Task Execution

Independent runnable tasks of a multithreaded guest SHALL be permitted to execute concurrently on distinct worker threads, so a guest SHALL be able to consume more than one CPU core at a time.

#### Scenario: CPU-bound tasks overlap

- **WHEN** a multithreaded guest runs two CPU-bound tasks with the pool providing at least two workers
- **THEN** both tasks SHALL make progress concurrently

### Requirement: Single-Flight Task Invariant

At most one worker SHALL execute a given task's future at a time; a task SHALL remain single-flight even though distinct tasks run in parallel.

#### Scenario: A polled task is not polled twice at once

- **WHEN** a task is being polled by one worker
- **THEN** no other worker SHALL poll that same task concurrently

### Requirement: Wake Delivery by Notify

When the host observes a wake condition for a parked task of a multithreaded guest, it SHALL deliver the wake by bumping the task's parking word (the host-observable mirror of the task's wake counter) and notifying the guest's shared wake word; parked workers resume in place, and the woken task SHALL be resumed by whichever worker claims it from the guest's shared run queue — there is no task-to-worker binding. The host SHALL NOT execute the guest's reactor as a unit on the waking thread.

#### Scenario: Parked task resumes in place

- **WHEN** the kernel poller advances a region generation for a task parked in an atomic wait
- **THEN** the runtime SHALL bump the task's parking word and notify the guest's shared wake word, and the task SHALL resume execution on the worker that claims it from the run queue

### Requirement: Per-Worker Fault Isolation

A trap raised while a worker executes the guest SHALL stop only that worker; the runtime SHALL then apply the process teardown policy for the guest. Remaining workers SHALL NOT continue executing a faulted instance's state.

#### Scenario: Faulting worker stops the process

- **WHEN** one worker traps while executing the guest
- **THEN** the runtime SHALL stop the guest's workers and reclaim the process per the teardown policy, rather than allowing other workers to run against a faulted instance

### Requirement: Concurrent Instance Execution Contract

The guest execution environment SHALL permit multiple host threads to execute one guest instance's code concurrently against the guest's shared linear memory, so distinct workers can run the guest at the same time.

#### Scenario: Workers share one instance concurrently

- **WHEN** two workers enter the same guest instance over its shared linear memory at the same time
- **THEN** both executions SHALL proceed safely without serialising the whole instance behind a single execution guard
