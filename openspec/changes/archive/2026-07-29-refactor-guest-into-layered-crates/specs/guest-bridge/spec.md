## ADDED Requirements

### Requirement: Bridge Guest Per External User
The system SHALL support a WASM guest (the "bridge") that terminates a single QUIC connection from an external client and transparently proxies `selium-wire` frames into shared-memory rings. A bridge SHALL be scoped to exactly one external user (one QUIC connection), multiplexing that user's channels as QUIC streams.

#### Scenario: External client opens a channel through the bridge
- **WHEN** an external client sends a frame on QUIC stream N with a topic correlation tag
- **THEN** the bridge SHALL write the identical frame to the corresponding shared-memory ring
- **AND** inner guest subscribers SHALL receive the frame indistinguishable from a locally-published frame

#### Scenario: Inner guest publishes to a channel
- **WHEN** an inner guest writes a frame to a shared-memory ring that the bridge subscribes to
- **THEN** the bridge SHALL read the frame and relay it to the external client over the corresponding QUIC stream

### Requirement: Transparent Frame Proxy
The bridge SHALL be a transparent relay: `selium-wire` frames SHALL pass through unchanged. Correlation IDs (frame tags), payload bytes, and frame flags SHALL be preserved end-to-end. The bridge SHALL NOT decode or re-encode payload contents.

#### Scenario: RPC correlation preserved through bridge
- **WHEN** an external client sends an RPC request with correlation tag 7
- **THEN** the inner guest receives the request with correlation tag 7
- **AND** the reply sent by the inner guest with tag 7 SHALL arrive at the external client with tag 7

### Requirement: Bridge Enforces Capability Grants
The bridge SHALL be subject to the same `CapabilityGrant`/`ResourceSelector` system as any other guest. It SHALL only be able to `attach` to channels that its grants permit. The runtime SHALL reject `attach_region` calls for resources the bridge's owning user has not been granted.

#### Scenario: Bridge attempts to attach to unauthorized channel
- **WHEN** a bridge calls `attach_region` for a channel the user lacks grants for
- **THEN** the host SHALL return an error, and the bridge SHALL propagate the failure to the external client

### Requirement: Bridge Failure Isolation
A bridge crash or supervisor kill SHALL only affect that user's sessions. Inner guests SHALL see `writer_count == 0` (or `PeerClosed`) on affected rings. Other bridges and inner guests SHALL be unaffected.

#### Scenario: Bridge crashes
- **WHEN** a bridge guest panics or is killed by the supervisor
- **THEN** inner guests subscribed to its rings SHALL detect `writer_count == 0` through normal disconnect detection
- **AND** no other bridge or guest SHALL be affected

### Requirement: Bridge Is Deployable Guest Code
The bridge SHALL be implemented as a standard WASM guest using `selium-guest` and `selium-quic`. It SHALL NOT require special runtime modifications beyond the existing UDP datagram hostcall.

#### Scenario: Bridge deployed via normal guest lifecycle
- **WHEN** the platform starts a bridge guest via `Process::start` with appropriate capability grants
- **THEN** the bridge SHALL initialize QUIC on the granted UDP socket, accept incoming streams, and relay frames to/from channels within its grants

### Requirement: Acceptor Guest Demux
The system SHALL support an acceptor guest (or a well-known listener) that owns the public UDP endpoint, accepts incoming QUIC connections, and spawns per-user bridge guests via `Process::start`. Connection demux SHALL happen in guest code, not in the runtime.

#### Scenario: New external connection arrives
- **WHEN** the acceptor guest accepts a new QUIC connection with authenticated user identity
- **THEN** it SHALL call `Process::start` to spawn a bridge guest with the user's capability grants, passing the QUIC connection handle
