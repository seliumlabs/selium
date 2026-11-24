# Spec Delta

## ADDED Requirements

### Requirement: Mailbox Pending State Is Authoritative
Mailbox pending-wake checks SHALL derive from atomic queue head and tail positions; the advisory flag SHALL NOT determine whether queued work remains.

#### Scenario: Wake races guest drain
- **WHEN** a host enqueues a wake while the guest drains and clears the advisory flag
- **THEN** the runtime SHALL observe head not equal to tail and continue polling until the wake is delivered

#### Scenario: Drained mailbox permits stall
- **WHEN** atomic head equals tail after all queued wakes are consumed
- **THEN** the runtime SHALL permit the guest reactor to stall

### Requirement: Timer Completion Is Deadline- And Error-Honest
A guest timer SHALL NOT complete before its deadline or convert a failed sleep into successful completion; sleep duration rounding SHALL preserve the at-or-after-deadline contract.

#### Scenario: Sub-millisecond remainder
- **WHEN** the remaining duration is positive but less than one millisecond
- **THEN** the guest SHALL request a duration that cannot complete before the deadline and SHALL recheck the deadline before finishing

#### Scenario: Sleep hostcall fails
- **WHEN** the host rejects or fails a timer's sleep operation
- **THEN** the timer SHALL surface the failure and SHALL NOT report successful completion early

#### Scenario: Spurious early wake
- **WHEN** a sleep completes before the requested deadline
- **THEN** the timer SHALL sleep for the remaining duration rather than complete
