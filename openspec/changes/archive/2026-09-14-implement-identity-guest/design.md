## Context

See proposal.md - Why. This design is shaped by the foundation already in place:

- TLS today is provisioned out-of-band: `quic-connector` loads a server `cert-pem` and per-tenant `client-ca-<tenant>` anchors from a `tls-certs` blob store, and builds its `ClientAnchorSet` once at startup.
- The authenticated identity is already derived as `ClientIdentity { tenant, fingerprint }`, fingerprint being SHA-256 of the leaf SPKI so grants keyed by key survive certificate rotation.
- The bridge-server resolves identity to grants through an interim `IdentityGrantMap` stub, with a comment explicitly awaiting "the identity guest's RPC surface."
- `Capability` grants are host-enforced via an admission matrix; `DelegateGrants` is the precedent for a bootstrap-only, non-conferable capability; `SystemRegistration` is the precedent for root-namespace registration.
- Foundation primitives: durable logs + blob stores (`Capability::Storage`), `ResourceKind::LiveTable` regions, `SystemGuestDescriptor` bootstrap with dependency-ordered readiness, rkyv-encoded hostcalls.

## Goals / Non-Goals

**Goals:**

- Keep keys host-held and expose minting as signing-oracle hostcalls with no private-key read path.
- Give the policy (tenant onboarding, minting, rotation, revocation, baseline grants) to a single WASM guest.
- Make trust-anchor and grant data flow as live tables, not static blobs.

**Non-Goals:**

- Accounting, metering, quotas, and rate limiting (parked for the accountant guest).
- Cross-host key distribution and multi-host anchor consistency (deferred to cluster work).
- Hardware-backed key storage (HSM/KMS/TPM) — the keyring is a host-owned primitive with a pluggable backing; in-process is fine for day 1.
- CRL/OCSP infrastructure — leaf revocation is expiry-driven plus grant removal.

## Decisions

### 1. Signing oracle hostcalls; keys never cross the boundary

**Decision:** The host holds a tenant-scoped keyring (root offline, intermediate online, per-tenant CA keys online) and exposes `SignTenantCa`, `SignUserCert`, and `RevokeCa`. The identity guest receives only public DER certificates.

**Rationale:** A compromised or buggy guest cannot exfiltrate key material it never receives. This is the dumb-host/smart-guest split applied to crypto: host provides signing + enforcement, guest provides policy.

**Alternative considered:** Keys in guest memory. Rejected — no hardware root of trust in a WASM guest, and co-resident guests raise exfiltration risk.

**Alternative considered:** A "secure blob store" the guest can read. Rejected — any read path for private keys is a leak path; a signing oracle with no read is strictly safer.

### 2. Depth-3 PKI, root offline

**Decision:** Offline root (ops-only bootstrap), online intermediate (signs tenant CAs), per-tenant CA keys (sign leafs). No root hostcall exists.

**Rationale:** Tenant onboarding is a common runtime operation, so the signing key for tenant CAs must be online; the root need not be. This contains a root compromise to "re-key the intermediate" rather than "distrust all tenants," and the 3-level shape is the standard managed-PKI answer.

**Alternative considered:** Online root. Rejected — root is the crown jewel; keep it in ops/HSM and off the guest call path entirely.

### 3. Identity-only mint authority with a tiered request surface

**Decision:** `MintCertificate` is bootstrap-provisioned and non-conferable (mirroring `DelegateGrants`); the identity guest is its sole holder. Everyone else asks identity over a request-exchange. Identity exposes an operator tier (tenant create/revoke, mint tenant CA) and a tenant tier (issue user cert, manage own principals).

**Rationale:** CA minting policy lives in exactly one place. Other guests (external-api, control) delegate as dumb orchestrators, matching DESIGN-INTENT.

**Alternative considered:** Mint as a grantable capability that trusted guests could hold. Rejected — policy scatters across every holder and audits become per-holder, the "smuggle policy into the host layer" smell.

### 4. Identity owns baseline grants; the bridge confers

**Decision:** Identity publishes `fingerprint -> baseline grants` as a live table. The bridge-server reads it per connection and confers (baseline ⊖ any future accountant narrowing) via `DelegateGrants`. Identity itself does not spawn user processes and therefore needs no `DelegateGrants`.

**Rationale:** Grants are already keyed by fingerprint at the bridge, so identity is the natural persistent owner; conferral stays where the host enforces it.

**Alternative considered:** Activity-guest-owned baseline grants. Rejected — the bridge needs a per-connection read; sending it through the metering guest's path muddies the boundary.

### 5. State as durable log, exposed as live tables

**Decision:** Tenant and principal registries live in identity-owned durable logs, replayed into live tables (`ResourceKind::LiveTable`): one anchor table for connectors, one grant table for the bridge. Anchor publication replaces the static `tls-certs` blob manifests.

**Rationale:** Matches the existing state-machine pattern (gate/observe → compute → write-through durable state → reconcile) and gives restart-based recovery for free.

**Alternative considered:** Connectors pull from identity over request-response. Rejected — live tables are the established pattern and give connectors push-style freshness for revocation.

### 6. Revocation semantics

**Decision:** User revocation is expiry-driven leaf TTL plus baseline-grant removal (an unexpired cert becomes inert once grants disappear). Tenant CA revocation removes the anchor from the published table, deletes the tenant CA key from the keyring, and connectors rebuild their union verifier; live connections survive until re-authentication.

**Rationale:** Fingerprint-keyed grants already make cert content orthogonal to entitlement, so "fire this user" is a grant-table write, not a CRL. Tenant CA revocation is rare and destructive, and anchor removal is the only honest way to stop a compromised tenant CA.

### 7. ABI surface

**Decision:** New `Capability::MintCertificate`; new `HostcallRequest`/`HostcallOutput` variants for `SignTenantCa`, `SignUserCert`, `RevokeCa`. Identity request/response message types (operator/tenant tiers) live in `selium-service`, not `selium-abi`, per the existing Host-Guest Boundary rule.

### 8. Bootstrap authority

**Decision:** Identity boots as a system guest holding `MintCertificate` (bootstrap-provisioned), `Storage` (registries), `SystemRegistration` (root/suitably-scoped registration), and its interface privileges. It registers a root-namespace serving route (e.g. `sel:///identity`). Readiness is satisfied when the host keyring is initialized, the intermediate is loaded, the registries are replayed, and the anchor/grant tables are live. Connector readiness depends on identity readiness (anchor table available).

## Risks / Trade-offs

- **[Online intermediate is a standing high-value target]** → Keep it host-held, revocable via `RevokeCa`; a compromised intermediate is recoverable by re-keying under the offline root. HSM/KMS backing later.
- **[Identity guest becomes a single point of failure]** → Durable-log state + restart-based recovery; the runtime supervisor restarts it.
- **[Connector anchor staleness across the cluster]** → Live-table push with verifier rebuild; cross-host propagation is deferred and addressed under cluster work.
- **[ABI enum growth]** → New variants are additive and rkyv-encoded; keep them minimal (three hostcalls, one capability).
- **[Scope of the mint capability needs care in the admission matrix]** → Reuse the `DelegateGrants` non-conferability rule verbatim; keep admission centralized in `capability-enforcement`.

## Migration Plan

1. Add `MintCertificate` and the three hostcall variants to `selium-abi`.
2. Add the host keyring (root/intermediate/tenant CA keys) and signing dispatch to the runtime, behind `MintCertificate` admission.
3. Add the `crates/guests/identity` crate: registries, replay, live tables, tiered request surface.
4. Switch `quic-connector` to consume the anchor live table and rebuild its verifier on change (replace static blob-manifest anchoring).
5. Replace the bridge-server's `IdentityGrantMap` stub with a read of the published grant table.
6. Wire identity bootstrap into `SystemGuestDescriptor` with connector/bridge readiness dependencies.
7. Validate single-host end to end (golden-path spine).

**Rollback:** This is pre-implementation design work; rollback means reverting these OpenSpec artifacts and deferring the change.

## Open Questions

- Exact CSR/SPKI encoding (PEM vs DER) for `SignUserCert` — implementation detail, does not change specs.
- Rotation cadence and default TTLs for tenant CAs and leafs — policy defaults, deferrable.
- HSM/KMS keyring binding interface — deferred; the keyring hides it behind the signing hostcalls.
