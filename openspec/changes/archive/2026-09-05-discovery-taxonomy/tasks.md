## 1. ABI: target classification and enumeration queries

- [x] 1.1 Add `class` (from the closed `ResourceClass` enum) and `labels: Vec<(String, String)>` fields to `ResourceTarget`, keeping rkyv round-trip intact; verify `cargo test -p selium-abi` passes with a new round-trip test
- [x] 1.2 Add prefix-listing and label-query variants to `DiscoveryRequest`, and a multi-target variant to `DiscoveryResponse`; verify encode/decode round-trips in `selium-abi` tests
- [x] 1.3 Downcase the `ResourceClass` variants into the typed URI segment vocabulary (`proc`, `region`, `queue`, …) as a shared mapping; verify the mapping test covers every variant

## 2. Runtime: schema, process nodes, and provenance

- [x] 2.1 Rewrite `registration_uris`/`queue_registration_uri` in `discovery.rs` to emit `sel://<tenant>/<type>/<id>` (principal tenant, singular segments, no purpose alias); verify the unit tests assert the new URIs
- [x] 2.2 Register a `sel://<tenant>/proc/<id>` node at spawn and revoke it in `cleanup_process_resources`; verify new tests for spawn-register and teardown-revoke
- [x] 2.3 Thread the serving tenant through `AllocRegion` (and the byte-channel allocation path) and authorize cross-tenant allocation via tenant-scoped delegation, denying otherwise; verify "system allocates for another tenant" and "unauthorized cross-tenant denied" tests
- [x] 2.4 Move well-known channel URIs to the root tenant (`sel:///dns/resolve` style) in bootstrap provisioning and teardown; verify the provision/revoke scenarios against the new URIs

## 3. Discovery guest: store and query rewrite

- [x] 3.1 Replace URI-parsing ownership (``extract_process_id``/`_sys` reservation) with tenant + class + id handling and the reserved-root rule; verify reserved namespace tests assert `sel:///` rejection for Tier-2
- [x] 3.2 Implement leaf-aliases: bare-name → `(type, id)` map, class-noun rejection, and reverse revocation when a target is revoked; verify alias-resolve and revoke-target-revokes-aliases tests
- [x] 3.3 Implement the label index and label-query handler; verify matching targets are returned and unmatched targets are not
- [x] 3.4 Expose prefix/`*` enumeration over RPC with tenant scoping; verify same-tenant enumeration and cross-tenant denial tests
- [x] 3.5 Wire tenant scoping through real caller metadata (remove the `None` stub) for exact and enumerated resolution; verify the tenant-scoped resolution scenarios
- [x] 3.6 Support opaque external-name registrations (store and match exact canonical keys without scheme/path interpretation); verify exact-match and non-equivalent-spelling tests

## 4. Connectors: external-name resolution

- [x] 4.1 In `http`/`dns`/`quic` route resolution, replace `sel-<proto>://` construction with normalization of the incoming address and an external-name lookup; verify route resolution tests use the new keys
- [x] 4.2 Remove `protocol_uri`/`is_protocol_scheme` usage from connector code paths; verify `uri.rs` compiles with those helpers deleted or reduced to internal normalization

## 5. Integration and validation

- [x] 5.1 Update runtime substrate tests and examples that reference `sel://_sys/…` or `sel-http://…` to the new grammar; verify the spine golden-path test stays green (`cargo test -p selium-runtime --test spine`)
- [x] 5.2 Add an end-to-end discovery test exercising spawn-node → allocate-region → alias → label-query → teardown-revoke; verify it passes in the runtime substrate
- [x] 5.3 Confirm `openspec validate discovery-taxonomy` is clean and the full workspace builds, clippy is clean, and all tests pass

## 6. Review follow-ups (hardening and spec alignment)

- [x] 6.1 Add `quic-connector` and `guest-bridge` delta specs (SNI → normalised bare names with eviction normalisation; bridge route → `sel://<tenant>/bridge`) and list both capabilities in the proposal; verify no main spec still mandates the retired `sel-quic://` grammar
- [x] 6.2 Authorize cross-tenant allocation for root principals (option B) alongside tenant-scoped delegation, in both `AllocRegion` and `HostQueueCreate`; verify `root_process_allocates_for_any_tenant_without_delegation` (the connector mTLS path) and a tenant-scoped denial test pass
- [x] 6.3 Add the serving-tenant parameter to `HostQueueCreate` (ABI + guest `create_for_tenant`), register queues under the principal tenant, and revoke them under that tenant at teardown; verify the queue provenance substrate test passes
- [x] 6.4 Require at least one `Tenant` selector on `DelegateGrants` grants in `validate_grants`; verify a selector-less grant is rejected at spawn and a tenant-scoped one admitted
- [x] 6.5 Authorize Tier-2 revocations: aliases and external names only, within the caller's tenant, for targets the caller owns; typed URIs `Forbidden`, unknown keys `NotFound`; verify cross-tenant, unowned, typed-URI, and unknown-key tests pass
- [x] 6.6 Make alias registration verify the claimed target exists and its class matches the owned resource (class-aware ownership table); verify existence and class-mismatch tests pass
- [x] 6.7 Fail closed when the caller's tenant lookup errors (deny instead of silently unscoping); verify the per-variant denial mapping is tested
- [x] 6.8 Make teardown staged and retryable: revocation entries removed only after a successful publish, failed stops retain the process authority, and `cleanup_failed_process` keeps the authority for a later retry; verify retry tests for both stop paths pass
- [x] 6.9 Normalise the raw SNI before cache eviction in the QUIC connector; verify the eviction-normalisation test passes
- [x] 6.10 Make the FlatBuffers discovery codec decode strictly (unknown class segment, unknown variant tag, missing Register/Found target are errors, not silent defaults); verify decode-error tests pass
