## MODIFIED Requirements

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

## ADDED Requirements

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
