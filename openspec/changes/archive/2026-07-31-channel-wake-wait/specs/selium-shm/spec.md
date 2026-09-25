# Spec Delta: selium-shm

## MODIFIED Requirements

### Requirement: Reader/Writer Wait Semantics

`Reader`, `BlockingReader`, `Writer`, and `BlockingWriter` SHALL NOT
return an unwakeable `Poll::Pending` and SHALL NOT busy-spin. Unmet
read/write conditions SHALL register the task for a generation-counter
wake before returning `Poll::Pending`.

#### Scenario: Pending implies registered

- **WHEN** any channel `poll_read` or `poll_write` returns `Poll::Pending`
- **THEN** the calling task is registered to be woken by a later
  generation bump or writer-count change

#### Scenario: Disconnect is observable

- **WHEN** the last writer on a channel disconnects
- **THEN** the generation counter is bumped so parked readers observe
  end-of-stream
