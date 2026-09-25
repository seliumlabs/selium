## ADDED Requirements

### Requirement: Gossip-Based Peer Discovery
The cluster coordination layer SHALL support gossip-based peer discovery for clusters that exceed the configured full-mesh threshold.

#### Scenario: Cluster exceeds full-mesh threshold
- **WHEN** the number of hosts exceeds the configured full-mesh threshold
- **THEN** cluster coordination SHALL use gossip-based peer discovery instead of requiring every host to maintain a direct connection to every other host

### Requirement: Gossip State Propagation
The cluster coordination layer SHALL propagate cluster state through gossip when gossip mode is active.

#### Scenario: Host state changes in gossip mode
- **WHEN** a host publishes updated membership or load state while gossip mode is active
- **THEN** the update SHALL be propagated to peer subsets until the cluster converges according to the gossip policy

### Requirement: Full-Mesh Small-Cluster Mode
The cluster coordination layer SHALL retain full-mesh coordination for clusters below the configured gossip activation threshold.

#### Scenario: Small cluster starts
- **WHEN** the cluster size is below the configured threshold
- **THEN** cluster coordination SHALL keep using full-mesh peer connectivity

### Requirement: Load Shedding
The cluster coordination layer SHALL provide a way for overloaded hosts to redirect clients or peers to healthier hosts.

#### Scenario: Host is overloaded
- **WHEN** a host cannot accept additional work due to load
- **THEN** it SHALL return or publish an alternative host candidate when one is available
