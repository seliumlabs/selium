# Proposal: Channel Replication for Durability

## Why

Day 1 Selium uses single-master channels without replication. If a host fails, channels on that host are lost. This proposal adds write-master/read-slaves replication for channels that need durability.

## What Changes

- Add replication configuration to channels (replication factor)
- Implement write-master pattern: all writes go to master, reads from any replica
- Implement leader election when master fails (using Raft)
- Apply backpressure during election (writers queue, re-queue on election complete)
- Writers detect election errors and re-queue via waker wired to election complete future

## Deferred

- Full quorum-based durability (future enhancement)
- Erasure coding (not needed for day 1)
- Automatic replica placement (manual config for day 1)
