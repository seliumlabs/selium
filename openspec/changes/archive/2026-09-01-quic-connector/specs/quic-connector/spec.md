## Purpose

Define the QUIC connector: a system guest that terminates external QUIC (TLS 1.3) at the edge and relays each bidirectional stream's bytes over shared-memory channels, so application guests serve QUIC byte transport with no network capabilities of their own.

## ADDED Requirements

### Requirement: Edge Termination of QUIC over TLS 1.3
The connector SHALL terminate QUIC (TLS 1.3) at the edge using a quinn endpoint over a UDP listener. The external wire encoding SHALL be real QUIC; any standards-compliant QUIC client SHALL be able to open a connection and streams to a connector-served guest without Selium client software.

#### Scenario: Client-grade connection
- **WHEN** an external client completes a QUIC handshake and opens a bidirectional stream
- **THEN** the connector SHALL accept the stream and forward its bytes to the serving guest

#### Scenario: Missing certificate material
- **WHEN** the connector starts without loadable certificate/key material
- **THEN** it SHALL fail loudly at startup and SHALL NOT accept QUIC connections

### Requirement: Opaque Byte-Stream Forwarding
The connector SHALL relay each bidirectional stream's bytes between the wire and the remote guest verbatim. It SHALL NOT parse, validate, transform, or encode application payloads. Wire formats (including FlatBuffers schemas) SHALL be defined by end users on top of the relayed byte streams.

#### Scenario: Arbitrary payload round-trips byte-identical
- **WHEN** an external client writes bytes on a stream
- **THEN** the guest SHALL receive exactly those bytes in order
- **AND** bytes written by the guest SHALL arrive at the client in order, unmodified

#### Scenario: User-defined flatbuffers schema is opaque to the connector
- **WHEN** the guest and client frame their traffic with a user-defined FlatBuffers schema
- **THEN** the connector SHALL forward the encoded bytes without inspecting or re-encoding them

### Requirement: SNI-Based Discovery Route Resolution
The connector SHALL resolve the serving guest for a connection from the QUIC handshake's server name indication (SNI), matching the name against `sel-quic://` URIs registered with discovery. The connector SHALL NOT hold a static routing table.

#### Scenario: Connection routed to registered guest
- **WHEN** a connection presents an SNI under a registered `sel-quic://` name
- **THEN** the connector SHALL forward every stream on that connection to the resolved guest

#### Scenario: Unknown SNI refused at the handshake
- **WHEN** no registration matches the presented SNI (or SNI is absent)
- **THEN** the connector SHALL refuse the connection at the handshake and SHALL NOT contact any app guest

### Requirement: Per-Stream Channel Isolation
Each accepted stream SHALL be relayed over its own shared-memory channel granted with `ExplicitResource` to exactly the connector and the serving guest. Bytes on one stream SHALL NOT be deliverable on another stream's channel.

#### Scenario: Concurrent streams do not cross
- **WHEN** one connection carries multiple bidirectional streams
- **THEN** each stream's bytes SHALL be relayed on a distinct channel to the same guest, in that stream's order

### Requirement: Zero-Network-Grant App Guests
App guests served by the connector SHALL require no `Network` capability grants — only channel attach grants scoped to their per-stream regions (recommended: `ExplicitResource` per stream). Broad shared-memory `UriPrefix` grants SHALL be documented as an anti-pattern for connector-served channels.

#### Scenario: App guest serves with no Network grant
- **WHEN** an app guest holding only channel attach grants is registered for a `sel-quic://` name
- **THEN** it SHALL receive and answer relayed byte streams successfully

#### Scenario: Ungranted third party cannot intercept
- **WHEN** a guest without a grant for a stream region attempts to attach to it
- **THEN** the runtime SHALL deny the attach

### Requirement: Edge Backpressure Honesty
The connector SHALL translate channel backpressure into QUIC flow control: when a stream's ring is full, the connector SHALL stop reading that stream until capacity frees, and SHALL NOT buffer unboundedly. Slow clients SHALL cause ring writers to park before the guest rather than buffering at the edge.

#### Scenario: Slow app guest
- **WHEN** a guest consumes stream bytes slower than the client sends and the ring fills
- **THEN** the connector SHALL pause reading the stream and resume on capacity, with no stream bytes lost

#### Scenario: Slow client
- **WHEN** the client reads slower than the guest writes
- **THEN** the guest's ring writes SHALL park (and the connector's ring reads suspend) until the client drains

### Requirement: Stream Lifecycle Fidelity
The connector SHALL propagate stream lifecycle end-to-end: a FIN from the client SHALL surface to the guest as channel close/EOF, and the guest or connector closing a stream SHALL close (or reset) the corresponding QUIC stream on the wire.

#### Scenario: Client finishes a stream
- **WHEN** the client closes a bidirectional stream (FIN)
- **THEN** the guest SHALL observe end-of-stream on that stream's channel

#### Scenario: Guest closes a stream
- **WHEN** the guest closes a stream's channel
- **THEN** the connector SHALL finish (or reset) the corresponding QUIC stream so the client observes the close
