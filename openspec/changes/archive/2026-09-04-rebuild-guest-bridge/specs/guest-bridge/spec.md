## REMOVED Requirements

### Requirement: Bridge Guest Per External User
**Reason**: The bridge is rebuilt around per-stream `bridge-channel` processes rather than a single process per external user. One process per user assumes one QUIC connection per user and pushes channel multiplexing into the bridge.
**Migration**: Use the per-stream `bridge-channel` model added below; an external client opens one QUIC stream per fabric channel it joins.

### Requirement: Acceptor Guest Demux
**Reason**: The acceptor role is replaced by the per-tenant `bridge-server`, which terminates nothing itself and is served by the existing `quic-connector` rather than owning a public UDP endpoint.
**Migration**: Deploy a `bridge-server` per tenant registered at `sel-quic://<tenant>/bridge`; the connector delivers authenticated streams to it.

## MODIFIED Requirements

### Requirement: Transparent Frame Proxy
A bridge-channel SHALL be a transparent relay: `selium-wire` frames SHALL pass through unchanged between the relayed QUIC stream and the fabric channel. Correlation IDs (frame tags), payload bytes, and frame flags SHALL be preserved end-to-end. The bridge-channel SHALL NOT decode or re-encode payload contents.

#### Scenario: RPC correlation preserved through bridge
- **WHEN** an external client sends an RPC request frame with correlation tag 7
- **THEN** the inner guest receives the request with correlation tag 7
- **AND** the reply the inner guest sends with tag 7 SHALL arrive at the external client with tag 7

### Requirement: Bridge Enforces Capability Grants
A bridge-channel SHALL be subject to the same `CapabilityGrant`/`ResourceSelector` system as any other guest, holding the grants of the client whose identity it was spawned for. The runtime SHALL reject `AttachRegion` calls for channels the client has not been granted, and the bridge-channel SHALL surface the denial to the external client as a termination frame.

#### Scenario: Bridge attempts to attach to unauthorized channel
- **WHEN** a bridge-channel calls `AttachRegion` for a channel the client lacks grants for
- **THEN** the host SHALL return an error
- **AND** the bridge-channel SHALL close the pipe, sending the failure to the external client

### Requirement: Bridge Failure Isolation
A bridge-channel crash or supervisor kill SHALL only affect that pipe's stream and its fabric-channel memberships. Inner guests SHALL see `writer_count == 0` (or `PeerClosed`) on affected rings. Other bridge-channels and inner guests SHALL be unaffected.

#### Scenario: Bridge crashes
- **WHEN** a bridge-channel panics or is killed by the supervisor
- **THEN** inner guests attached to its rings SHALL detect `writer_count == 0` through normal disconnect detection
- **AND** no other bridge-channel or guest SHALL be affected

### Requirement: Bridge Is Deployable Guest Code
The bridge-server and bridge-channel SHALL be implemented as standard WASM guests using `selium-guest`. They SHALL NOT depend on the deleted `selium-quic` crate; QUIC termination SHALL be provided by `quic-connector`. They SHALL NOT require special runtime modifications beyond the generic handoff metadata (`IncomingConnection.metadata`) and the `DelegateGrants` capability introduced by this change.

#### Scenario: Bridge deployed via normal guest lifecycle
- **WHEN** the platform starts a bridge-server and it spawns bridge-channel guests via `Process::start` holding the client's grants
- **THEN** the bridge-server receives authenticated stream handoffs and each bridge-channel initializes a pipe and relays frames to/from channels within its grants

## ADDED Requirements

### Requirement: Per-Tenant Bridge Server
The system SHALL support a `bridge-server` system guest deployed one per tenant. The bridge-server SHALL register a `sel-quic://<tenant>/bridge` serving route so the QUIC connector delivers bridge-bound connections to it as per-stream handoffs. The bridge-server SHALL NOT terminate QUIC itself and SHALL NOT relay stream bytes.

#### Scenario: Bridge traffic routed to the tenant's bridge-server
- **WHEN** an external client presents an SNI matching a tenant's registered bridge route
- **THEN** the connector SHALL deliver the connection's streams to that tenant's bridge-server

#### Scenario: Bridge-server holds no data plane
- **WHEN** a bridge-server receives a stream handoff
- **THEN** it SHALL forward the handoff to a bridge-channel process rather than relaying bytes itself

### Requirement: Per-Stream Bridge Channel Process
For each delivered stream, the bridge-server SHALL spawn exactly one `bridge-channel` process holding the resolved client grants and the stream's shared region id. A bridge-channel SHALL bridge exactly one QUIC stream to exactly one fabric channel.

#### Scenario: One bridge-channel per stream
- **WHEN** a connection carries N streams
- **THEN** the bridge-server SHALL spawn N bridge-channel processes, each responsible for one stream

### Requirement: Identity to Grant Resolution
The bridge-server SHALL resolve a handoff's authenticated TLS identity — tenant scope and key fingerprint — to a capability grant set before spawning. The identity's tenant scope SHALL match the bridge-server's own tenant; identities resolved for any other tenant SHALL be refused. When the identity is unknown, the bridge-server SHALL close the delivered stream and SHALL NOT spawn a bridge-channel.

#### Scenario: Known identity resolves grants and spawns
- **WHEN** a handoff carries an identity present in the identity source
- **THEN** the bridge-server SHALL spawn a bridge-channel holding that identity's grants

#### Scenario: Unknown identity refused
- **WHEN** a handoff carries an identity not present in the identity source
- **THEN** the bridge-server SHALL close the delivered stream without spawning a bridge-channel

#### Scenario: Cross-tenant identity refused
- **WHEN** a handoff carries an identity whose tenant scope differs from the bridge-server's own tenant
- **THEN** the bridge-server SHALL close the delivered stream without spawning a bridge-channel

### Requirement: Pinned Connector Handoffs
The bridge-server SHALL accept handoffs only from the registered `sel-quic` protocol handler (the QUIC connector), resolved from the runtime's bootstrap-authoritative handler registry. Handoffs from any other process — including a guest that resolves the bridge route via discovery and attaches the queue with forged identity metadata — SHALL be refused. The bridge-server SHALL fail loudly at startup when no handler is registered, rather than serving unpinned handoffs.

#### Scenario: Forged handoff refused
- **WHEN** a process other than the pinned connector delivers a handoff to the bridge-server's listener
- **THEN** the handoff SHALL be refused without spawning a bridge-channel, and the delivered region SHALL be closed so the sender observes EOF

### Requirement: Typed Pipe Handshake
Before relaying data frames, a bridge-channel SHALL read a typed handshake message naming the fabric channel to bridge (its discovery URI). Data frames SHALL be relayed only after a successful handshake.

#### Scenario: Client opens a pipe
- **WHEN** an external client sends a handshake message naming a channel URI on a stream
- **THEN** the bridge-channel SHALL resolve and attach that channel before relaying further frames

#### Scenario: Pipe instantiation fails
- **WHEN** the bridge-channel cannot resolve or attach the requested channel
- **THEN** it SHALL send a typed termination message describing the failure and close the stream

### Requirement: Discovery-Resolved Channel Binding
The bridge-channel SHALL resolve the handshake URI through discovery and bind the stream to the resolved channel. Resolution SHALL be tenant-scoped.

#### Scenario: URI resolves to a channel within the tenant
- **WHEN** the handshake names a URI that discovery resolves to a channel target the client is allowed to reach
- **THEN** the bridge-channel SHALL attach that channel and begin relaying frames

### Requirement: Self-Cleaning Pipe Teardown
Closing either end of a pipe SHALL tear down the whole pipe and terminate the bridge-channel: a client FIN SHALL drop the bridge-channel's fabric membership, and a fabric close (all inner writers and blocking readers gone) SHALL finish (or reset) the client's stream. The bridge-channel SHALL contribute exactly one counting writer and one blocking reader to the fabric ring so that inner guests observe its membership (and its death) through the ring's member counts, and so that it can observe the fabric closing around it.

#### Scenario: Client disconnects
- **WHEN** the external client closes its stream or connection
- **THEN** the bridge-channel SHALL close its fabric membership and terminate

#### Scenario: Fabric channel closes
- **WHEN** the fabric channel closes (all inner peers gone)
- **THEN** the bridge-channel SHALL finish or reset the client's stream and terminate

### Requirement: Bounded Spawn
The bridge-server SHALL enforce a bound on bridge-channel spawns (for example per-identity or per-tenant concurrency) so an external client cannot exhaust the scheduler by opening many streams.

#### Scenario: Spawn bound exceeded
- **WHEN** a client attempts to open more streams than the configured bound permits
- **THEN** the bridge-server SHALL refuse the additional streams
