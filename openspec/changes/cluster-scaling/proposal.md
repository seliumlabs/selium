**Status:** post-baseline proposal. The current baseline is single-host; no
multi-host transport is claimed by the implemented system. This change is kept
for the scaling design and is not part of the current release gate.

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
- Cross-data-centre replication
