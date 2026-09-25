# Proposal: Implement the Identity Guest for Selium arch3

## Why

Selium's PKI is currently static: tenant trust anchors are provisioned out-of-band into a blob store, and the per-tenant bridge-server resolves client identities to grants through an interim hard-coded stub. The platform needs a self-managed identity authority — a system guest that mints and rotates tenant CAs and user certificates, owns the baseline grant registry, and publishes trust anchors and grants to the edge — so tenant onboarding and user lifecycle become runtime operations rather than operator file management.

## What Changes

- **New `selium-identity` system guest** (`crates/guests/identity`, package `selium-identity`): owns CA and user-certificate lifecycle policy — tenant onboarding, minting, rotation, and revocation — over a depth-3 PKI (offline root, online intermediate, per-tenant CA keys).
- **Host-held signing oracle**: a tenant-scoped keyring in the host (root offline/HSM, intermediate online, per-tenant CA keys online) exposed through signing hostcalls. No private-key read path exists; certificates (public) cross the boundary, private keys never leave the host.
- **New ABI surface**: a bootstrap-provisioned, non-delegable `MintCertificate` capability gateing certificate signing, plus `SignTenantCa`, `SignUserCert`, and `RevokeCa` hostcalls with their outputs.
- **Durable identity state**: an identity-owned tenant registry and a principal registry (`fingerprint -> baseline grants`), replayed into live tables following the guest state-machine pattern.
- **Published live tables**: `client-ca-<tenant>` trust anchors consumed by the QUIC connector's union verifier, and `fingerprint -> baseline grants` consumed by the bridge-server at handoff conferral.
- **Tiered request surface**: an operator tier (tenant create/revoke, mint tenant CA) and a tenant tier (issue user cert, manage own principals), both reached through the identity guest as the sole mint authority.
- **Bridge integration**: the interim `IdentityGrantMap` stub in the bridge-server is replaced by reads from the identity-published grant table.

## Capabilities

### New Capabilities

- `selium-identity`: the identity guest — tenant CA and user-certificate lifecycle, tenant and principal registries, baseline grant ownership, trust-anchor and grant publication, tiered operator/tenant request surface, and revocation semantics, all over a host-held signing oracle.

### Modified Capabilities

- `selium-abi`: adds the `MintCertificate` capability variant and the `SignTenantCa` / `SignUserCert` / `RevokeCa` hostcall request and output variants.
- `capability-enforcement`: `MintCertificate` is bootstrap-provisioned and not conferable at spawn, mirroring `DelegateGrants`.
- `quic-connector`: trust anchors become live-table-sourced from the identity guest instead of statically provisioned blobs; the connector refreshes its client union verifier when anchors are added or removed (tenant-CA revocation propagation).

## Impact

- **New crate**: `crates/guests/identity` (`selium-identity`).
- **New test-support guest**: `guests/identity-onboard` (`selium-identity-onboard`): the operator-tier driver for the golden-path integration test — tenant onboarding, leaf issuance, grant recording, and the tenant-revocation leg — exercised over the identity guest's tiered RPC surface exactly as a platform control guest will.
- **`crates/abi`**: new `Capability` variant, new `HostcallRequest`/`HostcallOutput` variants (certificate signing, plus the `RecordResolvedRegionFor` resolve-recording variant that gives live-table consumers — the connector and the bridge — an authorisation basis to attach the identity guest's published regions).
- **`crates/runtime`**: tenant-scoped keyring, signing hostcall dispatch, `MintCertificate` admission, identity `SystemGuestDescriptor` bootstrap with dependency-ordered readiness.
- **`guests/connector-quic`**: anchor consumption from the identity live table, verifier rebuild on anchor add/remove (replaces static `tls-certs` blob manifest loading); mTLS remains opt-in — without an identity guest deployed the connector serves without client authentication and handoffs carry empty identity metadata, which identity-requiring guests (the bridge) refuse.
- **`guests/bridge`**: replaces the interim `IdentityGrantMap` stub with identity-published grant table reads (implementation-only; bridge behavior is unchanged).
- **`selium-service`**: identity request/response message types for the tiered operator/tenant surface (additive; message types live in `selium-service`, not `selium-abi`).
