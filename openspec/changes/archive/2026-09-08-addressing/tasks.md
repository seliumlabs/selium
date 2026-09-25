## 1. Addressing core

- [x] 1.1 Add path/label functions (`labels_from_path`, `path_from_labels`) to `selium_abi::uri` and verify unit tests cover reversal and non-DNS-safe rejection
- [x] 1.2 Add `resolve_wire_name(name) -> Option<(tenant, path)>` with longest-match domain strip and synthetic-label fallback, and verify unit tests cover `bridge.acme`, `bridge.example.com`, and `prod.http.example.com`
- [x] 1.3 Add the advisory domain-to-tenant table type + lookup to the discovery layer, and verify table tests cover unknown-domain and provisioned-domain cases

## 2. Discovery

- [x] 2.1 Record the registering process as route owner on `Register` and revoke by owner on exit, and verify an owner-exit test shows the route no longer resolves
- [x] 2.2 Resolve external wire names through `resolve_wire_name` in the discovery service, and verify a resolve test maps `bridge.acme` to `sel://acme/bridge`
- [x] 2.3 Refuse registrations under a domain not owned by the registering tenant, and verify a foreign-domain registration test is refused
- [x] 2.4 Add an OOB seed path for the domain table, and verify a seeded `example.com -> acme` entry resolves in discovery

## 3. Guest registration API

- [x] 3.1 Add `Context::serve` (path + target + root-service flag) deriving internal and wire names, and verify a unit test registers `["bridge"]` and resolves both `sel://acme/bridge` and `bridge.acme`
- [x] 3.2 Add a root-registration capability type in `selium-abi`, and verify capability admission tests reject root registration without the grant

## 4. Connectors

- [x] 4.1 Rewrite quic-connector `RouteResolver::resolve` over `resolve_wire_name`, and verify `resolve.rs` tests cover synthetic and registered domains plus unknown-SNI refusal
- [x] 4.2 Rewrite http-connector `RouteResolver::resolve` to derive tenant from Host via `resolve_wire_name` then resolve the path, and verify resolver tests cover host routing and not-found

## 5. System guest migration

- [x] 5.1 Migrate bridge-server to create its own listener and `serve(["bridge"])` with identity metadata decode intact, and verify `bridge_handoff` passes with no injected listener argument
- [x] 5.2 Migrate dns-connector to self-register `dns/resolve` under its root-registration grant, and verify dns spine tests pass without `well_known_uri` provisioning

## 6. Runtime de-provisioning and readiness gating

- [x] 6.1 Remove the `well_known_uri` descriptor field, queue minting, argument injection, and the `well_known_uris` map, and verify `cargo check -p selium-runtime` succeeds
- [x] 6.2 Remove the process-exit well-known revocation branch and confirm revocation is owner-driven via discovery, and verify a process-exit test shows routes revoked with no side map
- [x] 6.3 Gate readiness of role-declared system guests on discovery-observable self-registration (keep `mark_ready()` payload-free), and verify a test shows a system guest that never registers does not reach ready

## 7. Integration

- [x] 7.1 Update `bridge_handoff`, `quic_spine`, `quic_connector`, `http_connector`, and `discovery*` tests to the new model and verify they pass
- [x] 7.2 Run the golden path (`cargo test -p selium-runtime --test spine -- --ignored`) and verify it stays green

## 8. App-guest serve API migration (review follow-up)

- [x] 8.1 Migrate `QuicServe::bind` to `Context::serve` with path semantics (a bare single-label name no longer silently lands in the root namespace), and verify unit tests cover path splitting and wire-name projection
- [x] 8.2 Migrate `HttpServe::bind` / `HttpServeStream::bind` to `Context::serve` (URL-style external names no longer register), and verify unit tests cover path splitting
- [x] 8.3 Store single-segment serve routes as exact-key registrations preserving the caller's target (interface metadata intact) while keeping the typed-target cascade, and verify store tests cover interface survival and cascade revocation
- [x] 8.4 Route apex-host HTTP requests to the tenant's designated root service, and verify resolver tests cover apex candidate ordering and fallback
- [x] 8.5 Assert the runtime publishes owner-keyed `RevokeByOwner` on process exit in the discovery integration test (runtime level, no side map)
- [x] 8.6 Verify `ProcessCapability` / `RecordRegistration` hostcalls are denied for non-discovery callers
- [x] 8.7 Re-run the full suite and the golden paths (`spine`, `quic_spine`, `dns_spine`, `discovery` with `--ignored`) after the migration
