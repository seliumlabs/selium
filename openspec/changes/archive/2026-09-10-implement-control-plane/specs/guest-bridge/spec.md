## MODIFIED Requirements

### Requirement: Transparent Frame Proxy
A bridge-channel SHALL be a transparent relay: `selium-wire` frames SHALL pass through unchanged between the relayed QUIC stream and the bound target (the fabric channel, or the request/reply rings of a rendezvoused session). Correlation IDs (frame tags), payload bytes, and frame flags SHALL be preserved end-to-end. The bridge-channel SHALL NOT decode or re-encode payload contents.

#### Scenario: RPC correlation preserved through bridge
- **WHEN** an external client sends an RPC request frame with correlation tag 7
- **THEN** the inner guest receives the request with correlation tag 7
- **AND** the reply the inner guest sends with tag 7 SHALL arrive at the external client with tag 7

### Requirement: Per-Stream Bridge Channel Process
For each delivered stream, the bridge-server SHALL spawn exactly one `bridge-channel` process holding the resolved client grants and the stream's shared region id. A bridge-channel SHALL bridge exactly one QUIC stream to exactly one served target: one fabric channel (transparent splice) or one RPC session rendezvoused into a served host queue.

#### Scenario: One bridge-channel per stream
- **WHEN** a connection carries N streams
- **THEN** the bridge-server SHALL spawn N bridge-channel processes, each responsible for one stream

### Requirement: Typed Pipe Handshake
Before relaying data frames, a bridge-channel SHALL read a typed handshake message naming the fabric channel to bridge (its discovery URI). The handshake SHALL be deterministic: after reading the handshake, the bridge-channel SHALL send exactly one typed control reply — an acceptance frame once the target is resolved and the pipe is established (immediately before the relay begins), or a termination frame describing the failure (followed by stream teardown). Data frames SHALL be relayed only after the acceptance reply.

#### Scenario: Client opens a pipe
- **WHEN** an external client sends a handshake message naming a channel URI on a stream
- **THEN** the bridge-channel SHALL resolve the target, reply with an acceptance control frame, and only then relay further frames

#### Scenario: Pipe instantiation fails
- **WHEN** the bridge-channel cannot resolve or establish the requested target
- **THEN** it SHALL send a typed termination message describing the failure and close the stream

### Requirement: Discovery-Resolved Channel Binding
The bridge-channel SHALL resolve the handshake URI through discovery and bind the stream to the resolved target. Resolution SHALL be tenant-scoped. Binding SHALL dispatch on the resolved target's class: channel targets SHALL splice the stream into the resolved channel ring (transparent relay, unchanged); host-queue targets SHALL take the RPC rendezvous.

#### Scenario: URI resolves to a channel within the tenant
- **WHEN** the handshake names a URI that discovery resolves to a channel target the client is allowed to reach
- **THEN** the bridge-channel SHALL attach that channel and relay frames unchanged

#### Scenario: URI resolves to a served host queue
- **WHEN** the handshake names a URI that discovery resolves to a host-queue target the client is allowed to reach
- **THEN** the bridge-channel SHALL perform the RPC rendezvous (see Host-Queue RPC Rendezvous) instead of attaching the queue as a channel

### Requirement: Self-Cleaning Pipe Teardown
Closing either end of a pipe SHALL tear down the whole pipe and terminate the bridge-channel: a client FIN SHALL drop the bridge-channel's fabric membership, and a fabric close (all inner writers and blocking readers gone) SHALL finish (or reset) the client's stream. For rendezvoused sessions, teardown SHALL also free the bridge-allocated session region so the serving guest observes session end. The bridge-channel SHALL contribute exactly one counting writer and one blocking reader to the fabric ring so that inner guests observe its membership (and its death) through the ring's member counts, and so that it can observe the fabric closing around it.

#### Scenario: Client disconnects
- **WHEN** the external client closes its stream or connection
- **THEN** the bridge-channel SHALL close its fabric membership, free any bridge-allocated session region, and terminate

#### Scenario: Fabric channel closes
- **WHEN** the fabric channel closes (all inner peers gone)
- **THEN** the bridge-channel SHALL finish or reset the client's stream and terminate

## ADDED Requirements

### Requirement: Host-Queue RPC Rendezvous
For a handshake URI that resolves to a host-queue target, the bridge-channel SHALL establish an RPC session on the external client's behalf, mirroring the internal `rpc::connect` path: allocate a two-ring shared-memory session region, splice the client's stream into it (request frames stream → request ring; reply ring → stream), and enqueue the session's shared id into the served queue. The handoff metadata SHALL carry the resolved client identity once guest-spawned processes can receive pointer arguments (today guest `Process::start` carries integer arguments only); until then the metadata is empty and authorization rests on the runtime's capability enforcement at queue attach. The session region SHALL be freed on pipe teardown so the serving guest observes session end, mirroring an internal client's drop.

#### Scenario: External RPC client reaches a served host queue
- **WHEN** an external client sends a handshake naming a URI that resolves to a host-queue target
- **THEN** the bridge-channel SHALL allocate a session region, splice the client's stream into it, and enqueue the session into the served queue
- **AND** the serving guest SHALL accept the session and exchange typed RPC frames with the external client, correlation tags preserved

#### Scenario: Session ends with the client
- **WHEN** the external client closes its stream while a rendezvoused session is established
- **THEN** the bridge-channel SHALL free the session region and the serving guest SHALL observe session end, not a leaked session
