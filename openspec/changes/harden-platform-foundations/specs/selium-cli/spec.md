# Spec Delta

## MODIFIED Requirements

### Requirement: Bridge-Derived Connection

The `sel` CLI SHALL establish one QUIC connection to a caller-supplied connector address per invocation. It SHALL use the root bridge service name and root control route, trust a caller-supplied server root certificate, and present a caller-supplied client identity for mutual TLS. Tenant authority SHALL derive from the verified certificate, not a caller-asserted route or server name.

#### Scenario: Tenant drives server name and control route

- **WHEN** the CLI is invoked with a connector address and client identity
- **THEN** it SHALL connect with server name `bridge` and open control route `sel:///control`

#### Scenario: Client identity is presented

- **WHEN** `--client-cert` and `--client-key` are supplied
- **THEN** the connection SHALL present that identity for mutual TLS so the connector derives the client's tenant and fingerprint

#### Scenario: Real CLI invocation reaches the control plane

- **WHEN** the CLI subprocess connects through the real connector and bridge to the running control-plane guest
- **THEN** its single typed request and rendered result SHALL match the control-plane response and exit status
