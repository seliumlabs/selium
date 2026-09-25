# Design

## Context

See `proposal.md` — Why. The relevant current state:

- `quic-connector` runs as root (`tenant: None`) with a trust-anchor set that is the union of every tenant CA, and attaches a `ClientIdentity { tenant, fingerprint }` to every stream handoff it delivers to the bridge route. The connector needs no change.
- The bridge-server decodes that identity and pins its listener to the connector via `expect_sender`; the single-instance control-plane needs neither: it serves internal shared-memory `rpc::accept` sessions like `discovery`, and its per-session tenant is the delivering process's tenant.
- `self_info()` returns the process's tenant (`None` = platform/root); `process_tenant(pid)` exposes another process's tenant. The identity guest already derives an operator/tenant tier from the caller's process tenant.
- `validate_grants` rejects a selector-less `DelegateGrants` (root-wide delegation must be explicit, not vacuous), and the delegation fence only honours `Tenant` selectors.
- Durable logs and blob stores are open-or-create by global name with no ownership; the tenant boundary is the grant check plus quota attribution to the writing process's tenant.

## Goals / Non-Goals

**Goals:**
- Collapse per-tenant control-plane and bridge-server into single per-platform instances.
- Scope every control-plane and bridge operation by the requestor's tenant.
- Keep the host generic: tenant labels arise from the process authority, not from bootstrap topology.

**Non-Goals:**
- No per-tenant metering of control-plane writes (platform-attributed; tracked).
- No kernel-level durable-log ownership/ACL (optional hardening, tracked).
- No `Namespace::Root` over TLS, no platform root CA (deferred to clustering).
- No module-compilation guest here (deferred; blob manifest keying is chosen to stay compatible with it).

## Decisions

### 1. Single per-platform instances serving root routes

**Decision:** Both guests bootstrap with `tenant: None`. The bridge-server serves `sel:///bridge` (wire name `bridge`); the control-plane serves `sel:///control` (wire name `control`). Both acquire `SystemRegistration` so discovery permits root-namespace registration.

**Rationale:** One instance per platform removes N×2 duplication and gives the root namespace a control surface it could otherwise never obtain (the per-tenant guests refuse `self_info() == None`). Bare wire names follow from the addressing rule that a root-tenant route projects with no domain suffix.

**Alternatives:** Keep per-tenant guests and only add a root pair — rejected: preserves the duplication and the SNI↔tenant mismatch surface the single instance eliminates.

### 2. Process-owner requestor-tenant derivation

**Decision:** Each control-plane session's tenant is derived from the delivering process's process owner: `process_tenant(client_process_id)` → `Namespace::Tenant(t)` for a named tenant, `Namespace::Root` for an unset (root) tenant. The tenant is never read from handoff metadata or any other tenant the caller asserts for itself; a session is refused only when the process-tenant lookup itself fails.

**Rationale:** The runtime's process authority is the only non-forgeable tenant source. External clients arrive as a bridge-channel spawned under their authenticated tenant, so the bridge-channel's process owner *is* the client's tenant — the authorisation gap is closed by the bridge-channel carrying the verified tenant into the session, not by the control-plane re-parsing identity. Serving internal shared-memory sessions only, like `discovery`, keeps the control-plane surface independent of edge topology.

**Alternatives:** Parse `ClientIdentity` handoff metadata — rejected: it needs the connector in the control-plane path (pinning the listener to the connector, or forwarding identity through the bridge-channel) for no gain, and trusts sender-controlled metadata. Root tenant `None` unusable — rejected: root is a valid namespace.

### 3. `Namespace` and hierarchical grant matching

**Decision:** Add `Namespace { Root, Tenant(String) }` to `selium-abi`. Add `ResourceSelector::Namespace(Namespace)`: `Tenant(t)` matches tenant `t` exactly; `Root` matches root contexts. In the `DelegateGrants` delegation fence, a `Namespace::Root` selector admits child grants for any tenant (root ⊇ tenant). `validate_grants` admits a `Namespace::Root`-carrying `DelegateGrants` (so root-wide delegation stays explicit and grantable, not a vacuous empty selector). The bridge-server spawns each bridge-channel under the handoff identity's tenant (`ProcessStart.tenant`), gated by its root `DelegateGrants`; the control-plane widens `Storage` by dropping its `Tenant` selectors (class-scoped = any), which requires no new mechanism. A `ProcessStart` naming a tenant other than the parent's own — for a root parent as much as a tenant-scoped one — is admitted only when the parent holds an in-scope `DelegateGrants` grant: cross-tenant spawn authority is always a grant, never an accident of the parent's bootstrap tenant.

**Rationale:** The existing rule "an unscoped grant means any" already expresses platform-wide authority; `DelegateGrants` alone forbids empties to prevent accidental global delegation. A named `Root` scope keeps the intent explicit and honest, matching the project's grant-honesty invariant, without polluting the `Namespace` type with an `Any` that would be invalid on records and identities.

**Alternatives:** Empty-selector `DelegateGrants` — rejected (validated out). `Namespace::Any` — rejected (wildcard role invalid on record/id uses). Process-tenant shortcut (root guest implicitly delegates) — rejected: authority should be a grant, not an accident of bootstrap tenant. This holds at the spawn-tenant level too: a root parent may not tenant-scope a spawn without an in-scope `DelegateGrants` grant (the runtime's `resolve_spawn_tenant` denies it).

### 4. Single platform desired-state log

**Decision:** The control-plane keeps one `DurableLog` (platform-scoped). Each `DesiredStateRecord` is tagged with its tenant on append; replay rebuilds a `Namespace`-keyed projection. Admin reads go through the tiered RPC (`Status`/delegation scoped to the session's tenant), never the raw log. Per-tenant write metering and kernel-level log ownership are deferred.

**Rationale:** One log keeps the replay model unchanged and makes the root namespace a partition like any other. Partitioning the read model by tenant preserves isolation at the only surface a tenant can observe.

**Alternatives:** Per-tenant logs selected by the requestor's tenant — rejected: would need per-tenant `Storage` grants on names under a naming convention, and quota would still attribute writes to the platform tenant.

### 5. Tenant-prefixed module manifests

**Decision:** `set_manifest` keys become `<tenant>:<manifest>` in the single control-plane blob store.

**Rationale:** Avoids cross-tenant manifest collisions under one store, and remains compatible with the deferred module-compilation guest that will own validation and `.aot` compilation of uploads.

## Risks / Trade-offs

- **[Cross-tenant leak from a missed tenant derivation]** → Single process-owner derivation path used by the control-plane; per-tenant unit and integration tests assert isolation (acme cannot read beta's partition or spawn with beta's grants).
- **[Root-wide grants are highly privileged]** → Accepted; precedent exists (identity holds unscoped `MintCertificate`/`SystemRegistration`; the connector trusts every tenant CA). `DelegateGrants` widens via the explicit `Namespace::Root` selector, and stays bootstrap-provisioned only. The bridge-server's sole trusted job is already to spawn each bridge-channel under its verified identity's tenant.
- **[Platform-log writes are unmetered]** → Accepted for day 1; recorded as a deferred item so the accountant's per-tenant quotas are not silently bypassed.
- **[Breaking wire-name change for clients]** → Documented; the tenant now comes from the presented leaf, which is the security property being bought.
- **[Log privacy rests on grant placement, not kernel ACL]** → The control-plane is the sole bearer of its `Storage` grant; optional kernel durable-log ownership is tracked as hardening.

## Migration Plan

1. Land `Namespace` and `ResourceSelector::Namespace` in `selium-abi`; extend `validate_grants`, the delegation fence, and tenant-scoped process spawn in `selium-runtime`.
2. Convert the bridge-server to single-instance: root route, identity-derived tenant (each bridge-channel spawned under the identity's tenant), `Namespace::Root` `DelegateGrants`, `SystemRegistration`.
3. Convert the control-plane: root route, process-owner tenant derivation, tenant-tagged log records, partitioned read model, tenant-prefixed manifests.
4. Collapse the per-tenant descriptors in the spine/bootstrap-test builders to single descriptors; add per-tenant isolation tests.
5. Update the golden-path spine to dial the bare `bridge` name.

**Rollback:** This is pre-implementation planning; rollback is reverting these artifacts. Once actioned, restoring the per-tenant descriptors and reverting the two guest entrypoints reverses the change (the wire-name change is the user-visible break).

## Open Questions

1. External `Namespace::Root` identity — a platform CA and anchor entry so an operator CLI authenticates as root over TLS — deferred to clustering/inter-runtime coordination.
2. Per-tenant metering of control-plane writes; whether the append hostcall should accept an acting-tenant override.
3. Kernel-level durable-log ownership as hard privacy (vs. grant-placement privacy).
4. The module-compilation guest's exact interface (`.aot` output, validation policy).
