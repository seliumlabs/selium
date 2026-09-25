# Spec Delta: selium-guest

## MODIFIED Requirements

### Requirement: Reactor Parking and Wake Sources

The guest reactor SHALL stall (return to the host) when only
channel-waiting tasks remain, and SHALL resume tasks whose registered
generation counters advanced. Task wakes SHALL arrive via the mailbox or
an in-guest futex wait, never via self-scheduled repolling.

#### Scenario: Reactor stalls on channel waits

- **WHEN** all runnable tasks complete and remaining tasks wait on
  channel generation counters
- **THEN** `poll_reactor` returns rather than spinning, and the next
  generation bump re-runs it and resumes the waiters

### Requirement: Host-Clock Timer Firing

`Timer` SHALL complete via a host-enqueued mailbox wake at the deadline;
the guest SHALL NOT poll the clock in a loop to detect expiry.

#### Scenario: Deadline wake delivery

- **WHEN** a `Timer` deadline passes while the guest reactor is stalled
- **THEN** the host enqueues a task wake for the sleeping task and the
  guest reactor resumes to complete the timer
