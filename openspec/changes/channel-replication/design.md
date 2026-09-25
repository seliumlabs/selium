## Context

Day 1 channels have no replication. This proposal adds write-master/read-slaves replication for durability.

## Goals / Non-Goals

**Goals:**
- Add replication factor to channel config
- Implement write-master pattern
- Handle master failure with election
- Apply backpressure during election

**Non-Goals:**
- Quorum-based durability
- Erasure coding
- Auto replica placement

## Decisions

### Write-Master/Read-Slaves

All writes go to master. Reads can be served by any replica.

### Backpressure During Election

Writers buffer in queued_payload (from switchboard pattern). On election error, writers wait on waker wired to election complete future.

## Risks

- **[Complexity]** Election handling is complex → Keep simple, fail writes during election first
- **[Consistency]** Read-slaves may lag → Document eventual consistency, warn users

## Open Questions

1. How to select replica hosts?
2. What's the replication factor default?
