## Context

Two parallel addressing systems exist today and never reconcile (see proposal.md - Why). The pieces this design builds on: `selium_abi::uri` (the only shared addressing code; today holds `sel://` grammar plus `bare_external_name`/`https_external_name`), the discovery service's `Register/Resolve/Revoke` RPC with tenant-scoped validation, `Context::register/lookup`, `QuicServe::bind` (pins to `sel-quic`), the connectors' `RouteResolver`s (flat-key lookups), and the runtime's `well_known_uri` provisioning (queue minting + argument injection + a side-car `well_known_uris` map used only for revive-on-exit). The bridge-server dependency-injects its discovery `Context` and decodes handoff `ClientIdentity` metadata.

## Goals / Non-Goals

**Goals:**
- One resolver, shared by QUIC and HTTP, that both connectors call unchanged.
- Guests self-register from resources they create; the runtime stops minting queues.
- A domain-to-tenant table that routes and scopes names without authenticating.

**Non-Goals:**
- Domain ownership verification (DNS-01 etc.) — provisioning stays out-of-band.
- Issuing per-tenant TLS server certificates (tracked as an open question).
- Subdomain delegation between tenants (v1: a tenant owns the whole domain).
- Any change to channel/memory semantics; this is naming only.

## Decisions

### 1. Canonical namespace is the tenant tree; wire names are reversed labels
Internal paths are canonical (`sel://acme/bridge`); a wire name is `labels = reverse(path)` joined with dots under the tenant's domain. Chosen over (a) a translation registry keyed by both names — two places to update, and the drift the project is trying to kill — and (b) keeping two namespaces — which is the current confusion.

### 2. Domain table is advisory and provisioned OOB
A `domain -> tenant` table lives in the discovery layer, seeded the same way client certs are. It is consulted for routing and for refusing registration under unowned domains. It is not an authentication source: identity still comes from the mTLS certificate, and the bridge-server's cross-tenant refusal is unchanged. Chosen over an enforced authority because certification remains the trust root and enforcement adds an admin surface with no authentication gain.

### 3. A single resolver in `selium_abi::uri`
`resolve_wire_name(name) -> Option<(Tenant, Vec<Segment>)>`: strip the tenant domain (longest match against the domain table, defaulting to the bare tenant label), reverse the remaining labels. Both `RouteResolver`s call it, then resolve the resulting path. The per-protocol difference collapses to the socket. Synthetic tenant labels resolve without any table entry; they never touch public DNS, so they cannot collide with real domains. An apex host (bare registered domain, empty path) resolves to the tenant's designated root service; in the HTTP resolver the root service owns the domain's request paths (it handles the path itself), with tenant-namespace paths as the fallback when no root service is designated.

### 4. `Context::serve` replaces route registration; root registration is capability-gated
`serve(Serve { path, target, default })` derives internal + wire names from one declaration against the guest's own tenant. Registration in the root tenant (`sel:///…`) is allowed only with a system-registration capability, replacing the runtime's special-casing. The guest always creates its own resource. The app-guest `bind` helpers (`QuicServe`, `HttpServe`, `HttpServeStream`) delegate to `serve` with path semantics, so every serve-side registration goes through the one declaration. A named route (single- or multi-segment) is an exact-key registration of the caller's own target — resolution preserves interface metadata (e.g. streamed-HTTP markers) — and a single-segment route keeps the leaf-alias cascade backref, so revoking the underlying typed resource still revokes the route.

### 5. Delete the well-known machinery; discovery owns revocation
`well_known_uri` descriptor field, queue minting, argument injection, and the `well_known_uris` map are removed. The discovery service records the registering process as every route's owner and revokes on exit — revocation is owner-keyed in discovery, not sided in the runtime. Discovery's own bootstrap (feed + listener) remains the single Tier-0 exception.

### 6. Readiness gates registration, with no payload
Self-registration removes the guaranteed provisioning path, so admission must catch a system guest that never registers. Because `serve` records owner + route in discovery, the runtime can verify readiness by querying discovery for the guest's registrations — no guest-to-runtime data channel is needed, and no payload rides on readiness. `mark_ready()` stays a bare signal; readiness becomes "alive AND declared roles are discoverable". A ghost-but-alive guest stays addressable via its process node, and a wedged-but-registered guest is the supervisor's concern, not discovery's.

## Risks / Trade-offs

- [Reverse-label is a convention, and labels must be DNS-safe] → Enforce the reversal only inside `resolve_wire_name`; document that only named leaf aliases project.
- [Domain table adds a spoof/squat surface] → Advisory: cert still authenticates, bridge still refuses cross-tenant identities.
- [Wire names under a bare tenant label could conceptually collide with real DNS] → Synthetic names resolve cluster-internally only; add a reserved suffix only if they must cross a cluster boundary later.
- [Removing `well_known_uri` breaks descriptors and tests in-flight] → Land this as one change; update `bridge_handoff`, `quic_spine`, `quic_connector`, `http_connector`, and `discovery*` together.
- [A system guest may stay silent and never register] → Readiness gates on discoverable registration, so a role-declared guest that never registers never reaches ready and is handled by the existing readiness machinery; meanwhile its process node keeps it addressable.

## Migration Plan

1. Add `resolve_wire_name` + the domain table (additive, no behaviour change).
2. Point both connectors' resolvers at it (behaviour-preserving for existing synthetic names).
3. Add `serve` + the root-registration capability; migrate bridge-server and dns-connector to self-register.
4. Gate role-declared readiness on discoverable registration (no `mark_ready()` payload change).
5. Remove `well_known_uri` provisioning and the side-car map; update tests.
Each step is independently reversible until step 5; step 5 lands last, with tests updated in the same change.

## Open Questions

- How are per-tenant TLS server certificates provisioned (single wildcard vs. rustls `ResolvesServerCert`)? Affects the connector's cert config, not the addressing specs.
- Does the synthetic `<tenant>` label need a reserved suffix once wire names cross cluster boundaries?
