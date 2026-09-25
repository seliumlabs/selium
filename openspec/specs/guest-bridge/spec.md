## Purpose

Define the reference bridge guest that terminates external QUIC connections and transparently proxies `selium-wire` frames into shared-memory rings, enabling external clients to communicate with inner guests through the Selium fabric.

## Requirements

### Requirement: Transparent Frame Proxy
A bridge-channel SHALL be a transparent relay: `selium-wire` frames SHALL pass through unchanged between the relayed QUIC stream and the bound target (the fabric channel, or the request/reply rings of a rendezvoused session). Correlation IDs (frame tags), payload bytes, and frame flags SHALL be preserved end-to-end. The bridge-channel SHALL NOT decode or re-encode payload contents.

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

### Requirement: Per-Tenant Bridge Server
The system SHALL support a single per-platform `bridge-server` system guest serving every tenant. The bridge-server SHALL register a `sel:///bridge` serving route (a root-namespace leaf alias) so the QUIC connector delivers bridge-bound connections to it as per-stream handoffs, and SHALL derive each stream's tenant from the handoff's authenticated identity. The bridge-server SHALL NOT terminate QUIC itself and SHALL NOT relay stream bytes.

#### Scenario: Bridge traffic routed to the tenant's bridge-server
- **WHEN** an external client presents an SNI resolving to the `bridge` serving route
- **THEN** the connector SHALL deliver the connection's streams to the single bridge-server

#### Scenario: Bridge-server holds no data plane
- **WHEN** a bridge-server receives a stream handoff
- **THEN** it SHALL forward the handoff to a bridge-channel process rather than relaying bytes itself

### Requirement: Per-Stream Bridge Channel Process
For each delivered stream, the bridge-server SHALL spawn exactly one `bridge-channel` process holding the resolved client grants and the stream's shared region id. A bridge-channel SHALL bridge exactly one QUIC stream to exactly one served target: one fabric channel (transparent splice) or one RPC session rendezvoused into a served host queue.

#### Scenario: One bridge-channel per stream
- **WHEN** a connection carries N streams
- **THEN** the bridge-server SHALL spawn N bridge-channel processes, each responsible for one stream

### Requirement: Identity to Grant Resolution
The bridge-server SHALL resolve a handoff's authenticated TLS identity — tenant scope and key fingerprint — to a capability grant set before spawning. The tenant scope SHALL be taken from the identity itself, and the bridge-server SHALL confer and narrow grants for whichever tenant the identity carries; the bridge-server has no own tenant to restrict the identity to. When the identity is unknown, the bridge-server SHALL close the delivered stream and SHALL NOT spawn a bridge-channel.

#### Scenario: Known identity resolves grants and spawns
- **WHEN** a handoff carries an identity present in the identity source
- **THEN** the bridge-server SHALL spawn a bridge-channel holding that identity's grants, narrowed for that identity's tenant

#### Scenario: Unknown identity refused
- **WHEN** a handoff carries an identity not present in the identity source
- **THEN** the bridge-server SHALL close the delivered stream without spawning a bridge-channel

#### Scenario: Cross-tenant identity refused
- **WHEN** a handoff carries an authenticated identity for a tenant (for example `acme`)
- **THEN** the bridge-server SHALL NOT refuse it on cross-tenant grounds, because the single bridge-server is bound to no one tenant
- **AND** the bridge-server SHALL confer and narrow grants for the identity's own tenant and spawn a bridge-channel scoped to that tenant

### Requirement: Pinned Connector Handoffs
The bridge-server SHALL accept handoffs only from the registered `sel-quic` protocol handler (the QUIC connector), resolved from the runtime's bootstrap-authoritative handler registry. Handoffs from any other process — including a guest that resolves the bridge route via discovery and attaches the queue with forged identity metadata — SHALL be refused. The bridge-server SHALL fail loudly at startup when no handler is registered, rather than serving unpinned handoffs.

#### Scenario: Forged handoff refused
- **WHEN** a process other than the pinned connector delivers a handoff to the bridge-server's listener
- **THEN** the handoff SHALL be refused without spawning a bridge-channel, and the delivered region SHALL be closed so the sender observes EOF

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

### Requirement: Bounded Spawn
The bridge-server SHALL bound bridge-channel spawns through the host-enforced per-tenant process quota: a tenant's effective spawn bound SHALL be its accountant-authored process-quota ceiling. When the runtime denies a spawn because the client tenant's process quota is exhausted, the bridge-server SHALL refuse the handoff and close the delivered stream so the client observes the refusal, and SHALL NOT maintain its own spawn counter.

#### Scenario: Spawn bound exceeded
- **WHEN** a client attempts to open more streams than the client tenant's process quota permits
- **THEN** the bridge-server SHALL refuse the additional streams and close each delivered stream so the client observes EOF

#### Scenario: Denied spawn needs no guest-local bookkeeping
- **WHEN** the runtime denies a bridge-channel spawn with `QuotaExceeded`
- **THEN** the bridge-server SHALL attach-then-close the stream and SHALL require no guest-local spawn counter to handle the refusal

### Requirement: Host-Queue RPC Rendezvous
For a handshake URI that resolves to a host-queue target, the bridge-channel SHALL establish an RPC session on the external client's behalf, mirroring the internal `rpc::connect` path: allocate a two-ring shared-memory session region, splice the client's stream into it (request frames stream → request ring; reply ring → stream), and enqueue the session's shared id into the served queue. The handoff metadata SHALL carry the resolved client identity once guest-spawned processes can receive pointer arguments (today guest `Process::start` carries integer arguments only); until then the metadata is empty and authorization rests on the runtime's capability enforcement at queue attach. The session region SHALL be freed on pipe teardown so the serving guest observes session end, mirroring an internal client's drop.

#### Scenario: External RPC client reaches a served host queue
- **WHEN** an external client sends a handshake naming a URI that resolves to a host-queue target
- **THEN** the bridge-channel SHALL allocate a session region, splice the client's stream into it, and enqueue the session into the served queue
- **AND** the serving guest SHALL accept the session and exchange typed RPC frames with the external client, correlation tags preserved

#### Scenario: Session ends with the client
- **WHEN** the external client closes its stream while a rendezvoused session is established
- **THEN** the bridge-channel SHALL free the session region and the serving guest SHALL observe session end, not a leaked session

### Requirement: Accountant Narrowing at Conferral

The bridge-server SHALL fold the accountant's published narrowing state for the identity's tenant into the identity-published baseline grants before conferring a grant set on a spawned bridge-channel.

#### Scenario: Baseline narrowed before conferral

- **WHEN** the bridge-server resolves a handoff identity and the accountant has published narrowing state for the identity's tenant
- **THEN** the bridge-channel SHALL be spawned holding the baseline grants minus the narrowing

#### Scenario: No narrowing is a no-op

- **WHEN** the identity's tenant has no published narrowing
- **THEN** the bridge-channel SHALL receive the identity-published baseline grants unchanged

#### Scenario: Narrowing table unreachable fails open

- **WHEN** the bridge-server boots and the accountant's narrowing table route cannot be resolved or attached (the accountant is down or booting)
- **THEN** the bridge-server SHALL continue conferring identity-published baseline grants un-narrowed (fail-open), with a warning logged — consistent with the accepted v1 single-accountant SPOF; the accountant re-publishes its narrowing state at its own boot, and conferrals fold it in from the first sync after it returns