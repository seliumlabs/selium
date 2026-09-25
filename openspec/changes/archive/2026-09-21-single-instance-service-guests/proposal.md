# Proposal

## Why

The control-plane and bridge-server guests are instantiated one per tenant, yet both are required by every tenant — including the root namespace, which has no tenant to instantiate them under. That forces N×2 privileged guests and leaves a live authorisation gap: the control plane serves `control.<tenant>` without deriving its per-session tenant from the requestor, so a client leaf issued by another tenant can reach it. Making both guests single per-platform instances and deriving the control plane's tenant from the delivering process's tenant (the process owner) removes the per-tenant duplication and closes that gap.

## What Changes

- **BREAKING**: `selium-control-plane` becomes a single per-platform instance (tenant `None`) serving `sel:///control` (wire name `control`), instead of one instance per tenant serving `sel://<tenant>/control`.
- **BREAKING**: the bridge-server becomes a single per-platform instance serving `sel:///bridge` (wire name `bridge`) for all tenants, instead of one instance per tenant serving `sel://<tenant>/bridge`.
- The control-plane derives the requestor's tenant per session from the delivering process's tenant (the process owner) — external clients arrive as a bridge-channel spawned under their authenticated tenant — and scopes desired state, delegation, and resolution by that tenant.
- The control-plane desired state moves to a single platform-scoped durable log whose records carry the tenant, with a read model partitioned by tenant.
- Module-blob manifests become tenant-prefixed within the single control-plane blob store.
- `selium-abi` gains a `Namespace { Root, Tenant(String) }` requestor-tenant type, and `ResourceSelector::Namespace(namespace)` with hierarchical matching (Root admits any tenant under delegation), used to broaden `DelegateGrants` for the single-instance bridge-server.

## Capabilities

### New Capabilities

None.

### Modified Capabilities

- `control-plane`: root-namespace serving route, per-session requestor-tenant derivation (from the process owner), per-tenant desired-state records, tenant-scoped module manifests.
- `guest-bridge`: single per-platform bridge-server serving the root route; tenant derived from the requestor's identity rather than matching the server's own tenant.
- `capability-enforcement`: `DelegateGrants` delegation scope extended with a root/system namespace selector that admits child grants for any tenant.
- `addressing`: root/system-tenant routes project to bare wire names with no tenant domain.

## Impact

- **Code**: `guests/control-plane/src/lib.rs`, `guests/bridge/src/lib.rs`, `crates/abi/src/lib.rs` (`Namespace`, `ResourceSelector::Namespace`), `crates/runtime/src/process.rs` (`validate_grants`, delegation admission), `crates/runtime/src/hostcall.rs` (delegation fence, tenant-scoped spawn gating), the runtime integration-test descriptor builders, and `crates/cli` (bare `bridge` server name, `sel:///control` route, `--tenant` dropped).
- **Deployment**: the per-tenant control-plane/bridge descriptors collapse to one system guest each. Both acquire `SystemRegistration` (root-route registration); the bridge acquires a root-scoped `DelegateGrants` grant; the control-plane drops its tenant selectors.
- **Callers**: external clients dial the bare `bridge` wire name (or a platform domain) instead of `bridge.<tenant>`; the tenant now comes from the presented leaf, not the name dialled. The control surface is an internal shared-memory RPC surface reached through the bridge, not dialled directly.
- **Deferred**: per-tenant metering of control-plane writes (the single platform log is platform-attributed); a future module-compilation guest that validates and compiles uploads to `.aot`; `Namespace::Root` over TLS for inter-runtime/cluster coordination.
