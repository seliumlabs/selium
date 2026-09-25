## Context

Full mesh QUIC doesn't scale beyond small clusters. Need gossip-based approach.

## Goals / Non-Goals

**Goals:**
- Scale beyond O(n²) connections
- Maintain reasonable latency
- Preserve consistency guarantees

**Non-Goals:**
- Geographic routing
- Multi-region

## Decisions

### Gossip Protocol

Use gossip for peer discovery and state propagation. Each host knows a subset of peers, state spreads via gossip.

### Threshold for Gossip

Full mesh is fine for clusters < 10 hosts. Gossip activates above threshold.

## Risks

- **[Consistency]** Gossip is eventually consistent → Document latency expectations
- **[Complexity]** Gossip is complex to implement → Evaluate existing libraries

## Open Questions

1. What gossip library to use?
2. What's the right threshold?
3. How to handle partition scenarios?
