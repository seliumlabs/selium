## ADDED Requirements

### Requirement: Replication Configuration
Replicated channels SHALL expose configuration for replication factor and replica placement.

#### Scenario: Replicated channel configured
- **WHEN** a channel is configured with a replication factor greater than one
- **THEN** the channel SHALL record the intended number of replicas and the configured placement inputs

### Requirement: Write-Master Routing
Replicated channels SHALL route writes through the current write master.

#### Scenario: Writer sends payload
- **WHEN** a writer appends a payload to a replicated channel
- **THEN** the write SHALL be routed to the current write master before being replicated to read slaves

### Requirement: Read-Slave Reads
Replicated channels SHALL allow reads from replicas that are eligible to serve the requested data.

#### Scenario: Reader requests available payload
- **WHEN** a reader requests data that an eligible read slave has applied
- **THEN** the read MAY be served by that read slave rather than the write master

### Requirement: Master Election
Replicated channels SHALL elect a new write master when the current master fails.

#### Scenario: Write master fails
- **WHEN** the current write master becomes unavailable
- **THEN** the channel replication layer SHALL run election and select a replacement write master before accepting normal writes again

### Requirement: Election Backpressure
Replicated channels SHALL apply write backpressure while write-master election is unresolved.

#### Scenario: Writer attempts write during election
- **WHEN** a writer attempts to append while write-master election is in progress
- **THEN** the write SHALL be queued, rejected with a retryable election error, or resumed through a waker when election completes
