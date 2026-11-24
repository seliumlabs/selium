# Spec Delta

## ADDED Requirements

### Requirement: RPC Establishment Owns Resources Across Cancellation
From allocation until ownership transfers to an established client or server, an RPC session SHALL have cancellation-safe ownership; dropping a connect or accept future SHALL reclaim or deliberately transfer every region.

#### Scenario: Client connect is cancelled before rendezvous
- **WHEN** a client drops its connection future before sending or completing the rendezvous
- **THEN** the unestablished session regions SHALL be freed and no server-side owner SHALL be fabricated

#### Scenario: Server accept is cancelled after rendezvous
- **WHEN** a server drops its accept future after receiving session identifiers but before establishing its owning handle
- **THEN** the received session SHALL be closed or reclaimed without leaking mappings or leaving a peer parked indefinitely

#### Scenario: Cancellation after establishment
- **WHEN** cancellation occurs after ownership has transferred to an established RPC handle
- **THEN** normal handle drop semantics SHALL close the session exactly once
