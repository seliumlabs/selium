# Spec Delta

## MODIFIED Requirements

### Requirement: Depth-3 Certificate Hierarchy
The identity guest SHALL operate a three-level hierarchy: an offline root CA used only to establish the online intermediate, an online intermediate key that signs tenant CAs, and per-tenant CA keys that sign user leaf certificates. The root private key SHALL NOT be reachable through a guest hostcall or persisted in the runtime's ordinary online state directory.

#### Scenario: Tenant CA minted by the intermediate

- **WHEN** a new tenant CA is minted
- **THEN** its certificate SHALL be signed by the online intermediate key

#### Scenario: Root key unreachable from guests

- **WHEN** any guest inspects the available signing hostcalls
- **THEN** no operator SHALL sign with the root key and no hostcall SHALL reference it

#### Scenario: Online state does not contain the root private key

- **WHEN** the host generates or loads its persistent hierarchy
- **THEN** the state directory SHALL contain the root public certificate and online intermediate material but no root private-key file

## ADDED Requirements

### Requirement: Identity Mutations Are Durable Before Visible
Principal, grant, tenant, and domain mutations SHALL NOT be acknowledged or exposed as successfully applied until their durable records are committed; replay SHALL restore every published projection.

#### Scenario: Principal append fails
- **WHEN** a principal registry record cannot be encoded, appended, or synced
- **THEN** identity SHALL return an error and SHALL NOT update the principal or grants projection as though the mutation succeeded

#### Scenario: Discovery restarts after domain registration
- **WHEN** identity remains running while discovery loses and rebuilds its in-memory registry
- **THEN** identity SHALL republish its durably recorded domains within a bounded reconciliation interval
