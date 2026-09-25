## Why

Selium currently runs two addressing systems that never reconcile: hierarchical internal URIs (`sel://<tenant>/<path>`) registered by guests and the runtime, and flat opaque external names (`localhost`, `https://host/path`) registered by connectors. That split forces hand-rolled translation, special-cases domain-to-tenant mapping, duplicates resolver logic per protocol, and — worst — makes the runtime mint listener queues for system guests purely so it can register their well-known URIs. This change replaces both with one namespace: domains are aliases of the tenant tree, not a second system.

## What Changes

- One canonical address space: `sel://<tenant>/<segments...>`. A wire name is the same path reversed, joined with dots, under a tenant-owned domain.
- An advisory domain-to-tenant table (`example.com -> acme`), provisioned out-of-band like client certs. It routes and signals; it never authenticates.
- One shared resolver for QUIC (SNI), HTTP (Host), and future protocols: wire name -> tenant -> reversed path -> route.
- Guests create and register their own routes via a `serve` call, deriving both internal and wire names from one declaration.
- **BREAKING** Remove `well_known_uri` descriptor field, runtime queue minting, the injected listener argument, and the `well_known_uris` side-car map — replaced by guest self-registration with process-keyed revocation in discovery.
- Apex alias: a tenant may designate one path as its root service so a bare domain (`example.com`) resolves directly to it.
- Readiness gates self-registration: a system guest with a declared serving role is not admitted as ready until its registration is observable in discovery. `mark_ready()` stays a bare signal with no payload.

## Capabilities

### New Capabilities
- `addressing`: the unified addressing model — tenant-rooted namespace, path/label reversal, advisory domain table, and the single wire-name resolver.

### Modified Capabilities
- `discovery-registration`: registration becomes guest-driven (`serve`); resolution accepts wire names via the domain table; revocation is keyed by the owning process.
- `quic-connector`: SNI resolution switches from flat `bare_external_name` lookup to the unified resolver.
- `http-connector`: Host resolution switches from flat `https_external_name` lookup to the unified resolver.
- `guest-context`: `Context` exposes `serve` (register a named route from the guest's own resources).
- `selium-runtime`: well-known URI provisioning (queue minting, argument injection, side-car map) is removed.

## Impact

- Code: `crates/abi/src/uri.rs`, `crates/guest/src/context.rs`, `crates/guest/src/net/{quic,http}.rs` (the app-guest `bind` helpers migrate to `serve` with path semantics), `guests/connector-quic/src/resolve.rs`, `guests/connector-http/src/resolve.rs`, `crates/runtime/src/{bootstrap,process,runtime,config}.rs`, `guests/bridge`, `guests/discovery`.
- Discovery: registry stores routes with an owning process; a domain-to-tenant table is added (provisioned OOB, advisory).
- Tests: `bridge_handoff`, `quic_spine`, `quic_connector`, `http_connector`, `discovery*` updated to the new registration model.
- Dependencies: none (independent). Unblocks the `selium-client` change, which builds `connect(uri, addr)` ergonomics on top of this model.
