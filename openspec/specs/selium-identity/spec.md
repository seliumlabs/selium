# Selium Identity Specification

## Purpose

The identity guest is the platform's PKI policy authority: it mints, rotates, and revokes tenant CAs and user certificates through a host-held signing oracle, owns the tenant and principal registries (including baseline capability grants), and publishes trust anchors and grants to the edge so tenant onboarding and user lifecycle are runtime operations.

## Requirements

### Requirement: Host-Held Signing Oracle

The identity guest SHALL mint certificates by invoking host signing hostcalls that operate over a host-held keyring. No hostcall SHALL return private-key material to any guest. Tenant CA keys and the intermediate key SHALL be generated host-side and SHALL NOT cross the host/guest boundary. User leaf keys SHALL be client-generated: the identity guest SHALL receive only the client's public key or certificate signing request, and SHALL return the signed certificate.

#### Scenario: Signing returns certificates, never keys

- **WHEN** the identity guest mints a tenant CA or a user certificate
- **THEN** the hostcall SHALL return DER-encoded public certificates only
- **AND** no hostcall output SHALL contain private-key material

#### Scenario: Tenant CA key never reaches the guest

- **WHEN** a tenant CA is minted
- **THEN** the tenant CA private key SHALL be generated and retained host-side
- **AND** the identity guest SHALL observe only the public CA certificate

#### Scenario: User leaf signed from client-supplied SPKI

- **WHEN** a tenant requests a user certificate for a client-generated key
- **THEN** the identity guest SHALL submit the client's public key (SPKI) to the signing hostcall
- **AND** the client SHALL receive a leaf certificate verifiable under the tenant's CA without the identity guest ever holding the client's private key

### Requirement: Depth-3 Certificate Hierarchy

The identity guest SHALL operate a three-level hierarchy: an offline root CA held only for operator bootstrap, an online intermediate key that signs tenant CAs, and per-tenant CA keys that sign user leaf certificates. The root SHALL NOT be reachable through any guest-facing hostcall.

#### Scenario: Tenant CA minted by the intermediate

- **WHEN** a new tenant CA is minted
- **THEN** its certificate SHALL be signed by the online intermediate key

#### Scenario: Root key unreachable from guests

- **WHEN** any guest inspects the available signing hostcalls
- **THEN** no operator SHALL sign with the root key
- **AND** no hostcall SHALL reference the root key

### Requirement: Tenant CA Lifecycle

The identity guest SHALL mint, rotate, and revoke tenant CAs on operator-tier requests. Minting SHALL generate a host-held tenant CA key, sign it via the intermediate, record the tenant in the tenant registry, and publish the tenant's trust anchor to the anchor live table.

#### Scenario: Onboarding a tenant publishes its anchor

- **WHEN** the identity guest mints a new tenant CA for tenant "acme"
- **THEN** the tenant registry SHALL record "acme"
- **AND** the anchor live table SHALL gain a `client-ca-acme` entry carrying the tenant's public CA certificate

#### Scenario: Revoking a tenant CA removes its anchor

- **WHEN** the identity guest revokes tenant "acme"
- **THEN** the anchor table SHALL lose the `client-ca-acme` entry
- **AND** the host SHALL delete the tenant's CA key from the keyring

### Requirement: User Certificate Lifecycle

The identity guest SHALL issue short-TTL user leaf certificates for a tenant's principals from a client-supplied SPKI, and SHALL record the leaf's SPKI fingerprint in the principal registry. Leafs SHALL expire by time-to-live. User revocation SHALL be achieved either by leaf expiry or by removing the principal's baseline grants.

#### Scenario: Issuing a user cert records the fingerprint

- **WHEN** a user certificate is issued for a principal's public key
- **THEN** the principal registry SHALL record that key's SPKI fingerprint within the principal's tenant

#### Scenario: Short-TTL leaf expires

- **WHEN** a user leaf certificate reaches its time-to-live
- **THEN** the connector SHALL refuse a handshake that presents the expired leaf

### Requirement: Principal Registry and Baseline Grants

The identity guest SHALL durably own a principal registry mapping each principal's key fingerprint to its tenant and its baseline capability grant set, and SHALL publish a fingerprint-to-baseline-grants live table consumed by the per-tenant bridge-server at handoff conferral.

#### Scenario: Grant table reflects the registry

- **WHEN** the identity guest records a principal's baseline grants
- **THEN** the published grants table SHALL expose that fingerprint's grant set

#### Scenario: Unknown fingerprint is absent

- **WHEN** a bridge-server queries the grants table for an unregistered fingerprint
- **THEN** the table SHALL return no entry, causing the bridge to refuse the handoff

### Requirement: Tiered Request Surface

The identity guest SHALL expose two request tiers: an operator tier for tenant creation, revocation, and tenant CA minting; and a tenant tier for issuing that tenant's user certificates and managing that tenant's principals. A request outside the caller's tier SHALL be refused. Mint authority SHALL be exclusive to the identity guest.

#### Scenario: Tenant-tier request cannot mint a tenant CA

- **WHEN** a tenant-tier caller requests a tenant CA mint for its own tenant
- **THEN** the identity guest SHALL refuse the request

#### Scenario: Operator-tier request mints a tenant CA

- **WHEN** an operator-tier caller requests a tenant CA mint
- **THEN** the identity guest SHALL mint the tenant CA and record the tenant

### Requirement: Durable State and Replay

The tenant and principal registries SHALL be stored as durable logs owned by the identity guest and replayed into live tables, so identity state survives identity guest restart.

#### Scenario: Restart replays registries

- **WHEN** the identity guest restarts
- **THEN** it SHALL replay its tenant and principal registries from durable storage
- **AND** the published anchor and grant tables SHALL be restored from the replayed state
