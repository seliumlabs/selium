# Proposal: Cluster Scaling Beyond Full Mesh

## Why

Day 1 uses full mesh QUIC connections between hosts. This scales as O(n²), which becomes impractical with large clusters. This proposal addresses scaling for production workloads.

## What Changes

- Migrate from full mesh to gossip-based peer discovery
- Implement hierarchical clustering for large deployments
- Add connection pooling and connection reuse
- Implement load shedding for overwhelmed hosts

## Deferred

- Geographic routing
- Multi-region deployment
- Cross-datacenter replication
