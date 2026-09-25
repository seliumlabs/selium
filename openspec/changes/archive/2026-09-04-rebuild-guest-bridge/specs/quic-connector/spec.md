## MODIFIED Requirements

### Requirement: Edge Termination of QUIC over TLS 1.3
The connector SHALL terminate QUIC (TLS 1.3) at the edge using a quinn endpoint over a UDP listener. The external wire encoding SHALL be real QUIC. Client authentication is **opt-in**: when per-tenant trust anchors are configured, the connector SHALL complete a handshake only for a client presenting a certificate chain that verifies to a configured trust anchor, and other connections SHALL be refused before any guest is contacted; when no trust anchors are configured, the connector SHALL accept connections without client authentication. Configured-but-broken anchor material (unreadable or invalid) SHALL fail loudly at startup rather than silently downgrading to no client authentication. TLS 1.3 0-RTT early data SHALL remain disabled: early data is replayable and the connector relays stream bytes into the fabric under the authenticated identity. A client without Selium software but holding a certificate chained to a configured anchor SHALL be able to open streams to a connector-served guest.

#### Scenario: Client-grade connection
- **WHEN** an external client presents a trusted certificate completes a QUIC handshake and opens a bidirectional stream
- **THEN** the connector SHALL accept the stream and forward its bytes to the serving guest

#### Scenario: Untrusted client refused
- **WHEN** client trust anchors are configured and a client presents no certificate, or one outside the configured trust anchors
- **THEN** the connector SHALL refuse the connection without contacting any app guest

#### Scenario: mTLS disabled
- **WHEN** no client trust anchors are configured and a client presents no certificate
- **THEN** the connector SHALL complete the handshake and serve the connection without client authentication

#### Scenario: Missing certificate material
- **WHEN** the connector starts without loadable server certificate/key material, or with a configured but unreadable or invalid client anchor
- **THEN** it SHALL fail loudly at startup and SHALL NOT accept QUIC connections

## ADDED Requirements

### Requirement: Authenticated Client Identity on Handoff
The connector SHALL derive an authenticated identity for each accepted connection — the tenant scope implied by the trust anchor that verified the client certificate, together with a fingerprint of the client's public key — and SHALL attach that identity, via handoff metadata, to each stream delivered to the serving guest. When client authentication is disabled, handoffs SHALL carry empty metadata.

#### Scenario: Handoff carries verified identity
- **WHEN** the connector accepts a stream from a verified client and delivers it to a serving guest
- **THEN** the handoff metadata SHALL contain the client's verified tenant scope and key fingerprint

#### Scenario: Handoff without mTLS
- **WHEN** the connector accepts a stream with client authentication disabled and delivers it to a serving guest
- **THEN** the handoff metadata SHALL be empty
