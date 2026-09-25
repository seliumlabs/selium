## Context

Discovery is a system guest fed by a Tier-1 runtime event stream and queried over Tier-2 RPC. Its store is a `BTreeMap<URI, ResourceTarget>` plus a `(process_id, resource_id)` ownership table and a scheme→handler map. URIs today are `sel://_sys/proc/<pid>/regions/<id>` (+ a hardcoded purpose alias) for internal resources and `sel-http://<host>/<path>` for routes. Prefix matching already exists internally (`resolve_prefix` with component-aware `prefix_matches`) but is not exposed over RPC, and tenant scoping is stubbed — `resolve_exact_scoped` is always called with `None`.

Two load-bearing details shape the rewrite. First, `uri.rs` is the single source of URI truth (scheme classification, reserved prefix, host/segment matching) — collapsing the grammar collapses this file. Second, the runtime is the authority for resource lifetime (regions, queues, owners), so discovery must never become a second authority; it stays a *fed* index.

## Goals / Non-Goals

**Goals:**
- One deterministic internal grammar: `sel://<tenant>/<type>/<id>`, tenant as the authority, empty authority = root.
- Leaf aliases, classification labels, and tenant-scoped enumeration wired for real.
- Process nodes registered at spawn so no process is invisible.
- External addresses as opaque name keys, retiring the `sel-proto://` family.

**Non-Goals:**
- The accountant's audit trail, a runtime `ProcessInspect` hostcall (containment stays runtime-side, composed in the SDK), domain-binding authority verification, arbitrary hierarchy, and the dependency graph. All explicitly rejected in this change.

## Decisions

**D1 — Rigid schema with authority-as-tenant.** `sel://<tenant>/<type>/<id>`, `<type>` drawn from the closed `ResourceClass` set. Tenant is the URI authority; empty authority is the root/system tenant, unreservable by guests by construction (non-empty required). Alternative considered: tenant as the first path segment — rejected, it's a positional hack and breaks the "non-empty tenant at the type level" property.

**D2 — Leaf aliases with reverse revocation.** A bare single-segment name under a tenant (`sel://acme/proxy`) resolves to a typed target `(type, id)`; an alias never names another alias; class nouns are reserved so an alias can't shadow a type segment; revoking a target revokes its aliases. Alternative: positional alias substitution anywhere — rejected, it reintroduces alias→alias resolution we deliberately excluded.

**D3 — Principal provenance with root-principal exemption.** Resources mint under the serving tenant, not the allocating process's tenant. Cross-tenant allocation is authorized two ways: a **root principal** (no tenant) may mint for any tenant — connectors and other trusted edge infrastructure run as root, and the QUIC connector under mTLS mints per-stream channels under the authenticated client's tenant without per-tenant grants — or a **tenant-scoped delegation grant** (`DelegateGrants` + `Tenant(T)`). Delegation grants must carry a `Tenant` selector: a selector-less grant would vacuously match every tenant including root, so validation rejects it. Alternative: process provenance — rejected, it strands bridged regions under the root connector's namespace and breaks tenant walks. Alternative considered post-review: per-tenant grants on the connector — rejected, each new tenant would require a connector restart and the CLI has no grant-provisioning path.

**D4 — External names are opaque keys; `sel-proto://` is retired.** A guest binds its real address (`https://acme.com/path/`, or a bare hostname for SNI-only transport); the connector normalizes incoming traffic to the canonical key and looks it up. Discovery neither parses nor validates the scheme. Alternative: keep `sel-proto://` — rejected, it's a fake URL that re-encodes host+path and forces a second grammar.

**D5 — Labels for classification, not hierarchy or edges.** `labels: [(key,value)]` on the target plus a flat `label → targets` index answers "what processes are in deployment X" without re-opening multi-home paths or reintroducing the graph. Containment ("what resources does process X hold") deliberately stays in the runtime.

**D6 — Process nodes at spawn/teardown.** The runtime registers `sel://<tenant>/proc/<id>` on spawn and revokes it on cleanup, closing the "process without regions is invisible" gap.

**D7 — Tenant scoping wired, fail-closed.** Enumeration and exact resolution read the caller's tenant from RPC connection metadata instead of the `None` stub, so one tenant cannot list or resolve another's resources. A *verified absent* tenant (root/system principal) skips the check for backward compatibility; a *failed* lookup denies — reads disclose nothing, writes are refused — rather than silently treating the caller as unscoped.

## Risks / Trade-offs

- **[URI rename has a broad blast radius]** → The system is pre-stable; deploy host, guests, and connectors together. The rewrite is mechanical, not semantic.
- **[rkyv wire-format changes]** → `ResourceTarget` gains fields; `DiscoveryRequest`/`Response` gain variants; `HostQueueCreate` gains a serving tenant. Host and guest builds must be coordinated, as with any ABI change.
- **[Domain-binding authority is unenforced]** → Using real `https://…` keys makes "who may bind a domain" visibly unanswered. Recorded as a known gap; identity/DNS verification is a separate concern.
- **[Alias squatting]** → Bounded by "Tier-2 must own the target, register within its own tenant, and point at a currently-registered target of the owned class"; root registrations are Tier-1 only.
- **[Enumeration as capability]** → Tenant-scoped by default; root/system principals may cross-tenant.
- **[Two sources of truth]** → Discovery (index) vs runtime (containment) is a deliberate split: discovery never mirrors holder state; `ProcessInspect` remains the runtime's answer.
- **[Root-principal exemption widens cross-tenant minting]** → Any root process may now mint for any tenant without a selector grant. Accepted: root processes are bootstrap-provisioned infrastructure (connectors, discovery), not guest-reachable; a tenant-scoped process still needs explicit delegation.
- **[Teardown revocations can fail]** → The feed is a Drop channel (publishes do not fail in practice), but teardown bookkeeping is staged and stops are retryable regardless: a failed publish leaves the pending revocation staged and the stop erroring rather than silently skipping revokes.

## Migration Plan

1. `selium-abi`: `ResourceTarget` gains class + labels; add prefix/label query and multi-target response variants.
2. `selium-runtime`: `registration_uris` → new schema; process-node spawn/teardown registration; `AllocRegion` principal/tenant propagation with delegation authorization; well-known channels move to the root tenant.
3. `guests/discovery`: store rewrite (typed-id map, alias map with reverse revocation, label index, prefix/label query handlers, wired tenant scoping, opaque external names).
4. Connectors (`http`, `dns`, `quic`): normalize incoming addresses and resolve as external-name keys; delete `sel-proto://` route construction.
5. Docs and examples: `sel-http://…` → `https://…`, `sel://_sys/…` → `sel:///…`.

## Open Questions

- **External-name canonical form for SNI-only protocols**: bare normalized hostname vs a registered scheme (`quic://`?). Default: bare hostname.
- **Label query scope**: per-tenant only, or root-level cross-tenant listing for system principals. Default: per-tenant.
- **`ProcessInspect` hostcall**: out of this change; expected to land as a separate runtime change.
