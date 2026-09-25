# QUIC Connector Specification

## Purpose

Define the QUIC connector: a system guest that terminates external QUIC (TLS 1.3) at the edge and relays each bidirectional stream's bytes over shared-memory channels, so application guests serve QUIC byte transport with no network capabilities of their own.

## Requirements

### Requirement: Edge Termination of QUIC over TLS 1.3
The connector SHALL terminate QUIC (TLS 1.3) at the edge using a quinn endpoint over a UDP listener. The external wire encoding SHALL be real QUIC. Client authentication is **opt-in through identity deployment**: when the identity guest's anchor table is deployed, the connector SHALL complete a handshake only for a client presenting a certificate chain that verifies to an identity-published trust anchor, and other connections SHALL be refused before any guest is contacted; when no identity guest is deployed (no anchor-table route registered), the connector SHALL accept connections without client authentication, and stream handoffs SHALL carry empty identity metadata — the deployment shape for user guests deliberately serving public, unauthorised endpoints, which identity-requiring guests (such as the bridge) refuse. A registered but unusable anchor source SHALL fail closed — the connector keeps serving QUIC but refuses every client certificate — rather than silently downgrading to no client authentication. Unloadable server certificate/key material SHALL fail loudly at startup. TLS 1.3 0-RTT early data SHALL remain disabled: early data is replayable and the connector relays stream bytes into the fabric under the authenticated identity. A client without Selium software but holding a certificate chained to a published anchor SHALL be able to open streams to a connector-served guest. TLS 1.3 session resumption SHALL remain disabled (the connector issues no resumption tickets and stores no resumable sessions): resumption would skip client authentication on the resumed handshake, bypassing both mandatory client authentication and anchor revocation. Every connection SHALL run the full certificate verification.

#### Scenario: Client-grade connection
- **WHEN** an external client presents a trusted certificate completes a QUIC handshake and opens a bidirectional stream
- **THEN** the connector SHALL accept the stream and forward its bytes to the serving guest


#### Scenario: Resumed session cannot bypass client authentication

- **WHEN** a client attempts to resume a previously authenticated session whose tenant anchor has since been removed
- **THEN** the connector SHALL run the full certificate verification and refuse the connection

#### Scenario: Untrusted client refused
- **WHEN** client trust anchors are configured and a client presents no certificate, or one outside the configured trust anchors
- **THEN** the connector SHALL refuse the connection without contacting any app guest

#### Scenario: mTLS disabled
- **WHEN** no identity guest is deployed and a client presents no certificate
- **THEN** the connector SHALL complete the handshake and serve the connection without client authentication
- **AND** stream handoffs SHALL carry empty identity metadata, which identity-requiring serving guests SHALL refuse

#### Scenario: Missing server certificate material
- **WHEN** the connector starts without loadable server certificate/key material
- **THEN** it SHALL fail loudly at startup and SHALL NOT accept QUIC connections

#### Scenario: Broken anchor source fails closed
- **WHEN** the identity anchor table is registered but cannot be attached
- **THEN** the connector SHALL refuse every client certificate rather than serve unauthenticated connections

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
The connector SHALL resolve the serving guest for a connection from the QUIC handshake's server name indication (SNI) through the unified addressing resolver: derive the tenant from the domain-to-tenant table (or the synthetic tenant label), reverse the remaining labels into a path, and resolve that path via discovery. The connector SHALL NOT hold a static routing table. The connector SHALL normalise the SNI identically when resolving and when evicting a cached route, so a stale route cannot survive eviction under a differently-spelled SNI.

#### Scenario: Connection routed to registered guest
- **WHEN** a connection presents an SNI that derives a tenant and reverses to a registered path
- **THEN** the connector SHALL forward every stream on that connection to the resolved guest

#### Scenario: Registered domain SNI
- **WHEN** a connection presents SNI `bridge.example.com` and `example.com -> acme` is provisioned
- **THEN** the connector SHALL resolve to the same `acme` bridge route it resolves for `bridge.acme`

#### Scenario: Unknown SNI refused at the handshake
- **WHEN** the SNI derives no tenant, resolves to no route, or is absent
- **THEN** the connector SHALL refuse the connection at the handshake and SHALL NOT contact any app guest

#### Scenario: Cache eviction normalises the raw SNI
- **WHEN** a route cached under a normalised name is evicted using a raw (mixed-case or trailing-dot) spelling of the same SNI
- **THEN** the cached entry SHALL be removed, so the next connection re-resolves

### Requirement: Per-Stream Channel Isolation
Each accepted stream SHALL be relayed over its own shared-memory channel granted with `ExplicitResource` to exactly the connector and the serving guest. Bytes on one stream SHALL NOT be deliverable on another stream's channel.

#### Scenario: Concurrent streams do not cross
- **WHEN** one connection carries multiple bidirectional streams
- **THEN** each stream's bytes SHALL be relayed on a distinct channel to the same guest, in that stream's order

### Requirement: Zero-Network-Grant App Guests
App guests served by the connector SHALL require no `Network` capability grants — only channel attach grants scoped to their per-stream regions (recommended: `ExplicitResource` per stream). Broad shared-memory `UriPrefix` grants SHALL be documented as an anti-pattern for connector-served channels.

#### Scenario: App guest serves with no Network grant
- **WHEN** an app guest holding only channel attach grants is registered for a bare external name
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

### Requirement: Authenticated Client Identity on Handoff
The connector SHALL derive an authenticated identity for each accepted connection — the tenant scope implied by the trust anchor that verified the client certificate, together with a fingerprint of the client's public key — and SHALL attach that identity, via handoff metadata, to each stream delivered to the serving guest. When client authentication is disabled, handoffs SHALL carry empty metadata.

#### Scenario: Handoff carries verified identity
- **WHEN** the connector accepts a stream from a verified client and delivers it to a serving guest
- **THEN** the handoff metadata SHALL contain the client's verified tenant scope and key fingerprint

#### Scenario: Handoff without mTLS
- **WHEN** the connector accepts a stream with client authentication disabled and delivers it to a serving guest
- **THEN** the handoff metadata SHALL be empty

### Requirement: Identity-Published Trust Anchors and Runtime Refresh

When the identity guest is deployed, the connector SHALL source its per-tenant client trust anchors from the anchor live table published by the identity guest, rather than statically provisioned blob manifests. When the anchor table changes — an anchor added on tenant onboarding or removed on tenant revocation — the connector SHALL rebuild its client union verifier so the change takes effect for the next handshake. The connector SHALL refuse to serve client-authenticated connections before receiving the identity guest's initial anchor state, rather than silently downgrading to no client authentication.

#### Scenario: New tenant anchor propagates

- **WHEN** the identity guest publishes a new tenant's trust anchor
- **THEN** the connector SHALL rebuild its union verifier and accept certificates chained to the new anchor

#### Scenario: Revoked tenant anchor propagates

- **WHEN** the identity guest removes a tenant's trust anchor
- **THEN** the connector SHALL rebuild its union verifier without that anchor
- **AND** the next handshake presenting a certificate chained to the removed anchor SHALL be refused

#### Scenario: Live connections survive revocation until re-auth

- **WHEN** a tenant anchor is removed while a verified connection is already open
- **THEN** the established connection SHALL remain valid until re-authentication

#### Scenario: Connector refuses to serve before anchor state arrives

- **WHEN** the connector starts before the identity guest has published its anchor table
- **THEN** the connector SHALL NOT accept client-authenticated connections until the initial anchor state is received

### Requirement: Per-Connection Bidirectional Stream Cap
The connector SHALL cap the number of concurrent bidirectional streams a connection may open, by configuring the QUIC endpoint's per-connection bidirectional stream limit, so a single connection cannot fan out into an unbounded number of per-stream channels. Streams beyond the cap SHALL NOT be accepted and SHALL NOT cause a per-stream region allocation.

#### Scenario: Connection exceeds the stream cap
- **WHEN** a client opens more concurrent bidirectional streams than the configured per-connection cap
- **THEN** the connector SHALL refuse the excess streams before allocating a per-stream channel

### Requirement: Per-Tenant Stream Admission Rate Limit
The connector SHALL rate-limit new stream admissions per tenant using a token bucket with a deployable-default rate and burst (the exact values are operator configuration, not spec behaviour), keyed by the authenticated client's tenant, falling back to the resolved serving tenant when client authentication is disabled. When the bucket is exhausted, the connector SHALL refuse the stream cheaply — resetting the stream with a distinct error code while keeping the connection — before allocating a per-stream channel, so a stream flood costs the least possible before reaching a serving guest.

#### Scenario: Flood above the rate is refused cheaply
- **WHEN** a tenant opens streams faster than the admission rate and the bucket is empty
- **THEN** the connector SHALL refuse the excess streams before allocating their channels

#### Scenario: Unauthenticated endpoint falls back to the serving tenant
- **WHEN** client authentication is disabled and a client opens streams over the admission rate
- **THEN** the connector SHALL rate-limit against the resolved serving tenant's bucket
