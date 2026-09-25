## 1. ABI: handoff metadata and delegation capability

- [x] 1.1 Add an optional opaque metadata field to the `HostQueueSend` and `HostQueueRecv` request variants, ensuring the payload survives rkyv round-trip; verify `cargo test -p selium-abi` passes, including a new round-trip test for the metadata field
- [x] 1.2 Add the `Capability::DelegateGrants` variant; verify the `Capability` enum round-trips and existing `matches`/`allows` tests still pass with `cargo test -p selium-abi`

## 2. Runtime: surface metadata and enforce delegation

- [x] 2.1 Thread `HostQueueSend` metadata through to `IncomingConnection.metadata`, and empty metadata when the sender omits it; verify new runtime tests for "metadata delivered" and "no metadata" (matching the resource-handshake delta scenarios)
- [x] 2.2 Add the delegation branch to `validate_child_grants`: when the parent holds a `DelegateGrants` grant matching the child's tenant, skip the subset check but still run well-formedness admission; verify three new tests — delegator-within-tenant spawns, non-delegator denied, cross-tenant delegation denied
- [x] 2.3 Verify a spawned bridge-channel inherits the bridge-server's tenant in process authority (children inherit parent tenant); assert with a bootstrap/spawn test reading `process_tenant`

## 3. Connector: mTLS client authentication and identity on handoff

- [x] 3.1 Load per-tenant client-verification trust anchors from the existing TLS blob store and build the quinn server config with a client verifier; mTLS is opt-in (no anchors configured → serve without client auth; configured-but-broken anchors fail loudly); verify config-build unit tests for present, absent, and invalid anchors
- [x] 3.2 Refuse connections that present no client certificate or one outside the configured anchors, before any app guest is contacted; verify a handshake seam test (trusted accepted, untrusted refused) mirroring the existing `tests/handshake.rs`
- [x] 3.3 Extract the client identity from `handshake_data`: tenant scope from the verifying anchor plus the leaf certificate SPKI fingerprint; verify with unit tests over fixture certificates
- [x] 3.4 Attach the identity to each stream handoff via `HostQueueSend` metadata in `handle_connection`; verify the "Handoff carries verified identity" scenario through the native test seam
- [x] 3.5 Document the mTLS-is-endpoint-global effect (single endpoint client-auth) in the connector docs per the design open question; verify the quic-connector spec scenarios remain satisfiable

## 4. bridge-server guest

- [x] 4.1 Reactivate `guests/bridge` as the `bridge-server` crate and restore workspace membership (`crate-type = ["cdylib"]`); verify `cargo build --target wasm32-unknown-unknown -p <bridge-server>` succeeds
- [x] 4.2 Register the `sel-quic://<tenant>/bridge` route: create a `ResourceListener`, register the URI with discovery, receive per-stream handoffs without attaching; verify bind registers with discovery (mirror `QuicServe::bind`)
- [x] 4.3 Resolve `IncomingConnection.metadata` to a client identity and map it to grants via the identity `HashMap` stub; verify lookup unit tests (known hit, unknown miss)
- [x] 4.4 Spawn a `bridge-channel(shared_id, grants)` on each handoff, passing the client's grants plus `ExplicitResource(shared_id)`; verify the spawn succeeds under the `DelegateGrants` path from 2.2
- [x] 4.5 Refuse unknown identities by attaching then closing the delivered region; verify the connector observes EOF and FINs the client stream
- [x] 4.6 Enforce a bounded spawn (concurrency/rate) and refuse overflow streams; verify a test that exceeds the bound is refused

## 5. bridge-channel guest

- [x] 5.1 Scaffold the `bridge-channel` crate (cdylib) and add to workspace members; verify wasm32 build succeeds
- [x] 5.2 Attach the delivered region as a `ByteStream` (`attach_blocking`) from the entrypoint arg; verify byte round-trip against a connector-style peer half (mirror the existing `stream.rs` test)
- [x] 5.3 Read the typed handshake frame naming a channel URI, resolve it via discovery, and `AttachRegion` the ring; verify the resolve-and-attach happy path with a test
- [x] 5.4 On denied attach, send a typed termination frame and close the stream; verify the "Bridge attempts to attach to unauthorized channel" scenario
- [x] 5.5 Splice frames stream⇄ring using `FramedRead`/`FramedWrite` over the byte channel and `ShmTransport`, preserving tag/flags/payload; verify an RPC correlation tag preserved end-to-end (tag 7 in/out)
- [x] 5.6 Tear down the whole pipe on either half closing — client FIN drops the fabric membership and exits, fabric close finishes/resets the stream and exits; verify both directions (incl. a quiet fabric channel after client disconnect)

## 6. Integration and validation

- [x] 6.1 Build a runtime substrate test wiring connector → bridge-server → bridge-channel → inner guest, driving a real QUIC client through open-stream → handshake → tagged round-trip; verify the golden "external client joins the fabric" flow
- [x] 6.2 Verify failure isolation: killing a bridge-channel mid-stream makes inner guests observe `writer_count == 0` without affecting other pipes
- [x] 6.3 Ensure the full workspace builds, clippy is clean, and all crates' tests pass; verify `cargo test --workspace` and `cargo clippy --workspace` succeed
- [x] 6.4 Confirm planning artifacts are consistent; verify `openspec validate rebuild-guest-bridge` reports the change valid

## 7. Review hardening and verification uplift

Follow-up to the critical review of the completed changeset. Fixes are implemented and verified in the same pass.

- [x] 7.1 Tighten the delegation branch in `validate_child_grants`: delegation admits a child grant set only when **every** child grant carries an in-scope `Tenant` selector (unscoped grants fall through to the subset check); verify a mixed tenant-scoped + unscoped grant set is denied
- [x] 7.2 Deny `DelegateGrants` conferment outright: a spawn whose child grants include `DelegateGrants` is refused even when the parent holds it; verify a re-delegation spawn is denied
- [x] 7.3 Pin handoff senders: add `ResolveProtocolHandler` + `SelfInfo` hostcalls; `ResourceListener::expect_sender` refuses handoffs from unpinned senders (attach-then-close); `QuicServe`/`HttpServe`/`HttpStreamServe::bind` pin to the scheme handler and fail closed; verify handler-resolution and oversized-metadata hostcall tests
- [x] 7.4 Bridge-server: pin the connector, refuse identities whose tenant differs from its own (via `SelfInfo`), release the spawn budget slot on failed spawns, and tenant-scope the conferred `ExplicitResource` grant; verify budget-release and attach-then-close-EOF unit tests
- [x] 7.5 Bound handoff metadata (`METADATA_MAX_BYTES`, 4 KiB) at the runtime hostcall and guest sender; verify oversized sends are rejected with a malformed-payload error
- [x] 7.6 Disable TLS 1.3 0-RTT early data on the connector (`max_early_data_size = 0`); early data is replayable and is relayed into the fabric under the authenticated identity
- [x] 7.7 Make mTLS opt-in (no anchors → no client auth, restoring the pre-mTLS spine flow; anchors → mandatory) and update the wasm spine test's connector behaviour accordingly; verify a certless client is accepted with mTLS off and refused with mTLS on
- [x] 7.8 Fix the fabric-close teardown defect: the pipe's ring adapters are split read/write (exactly one counting writer + one blocking reader on the fabric ring) and the fabric→client pump tears down when only the pipe's own members remain; verify the fabric-close direction (final frame relayed, then client stream finished, pipe terminated)
- [x] 7.9 Uplift failure-isolation verification: kill a live bridge pipe mid-stream and verify inner guests observe the membership release while a sibling pipe keeps relaying; uplift the golden substrate flow to bootstrap the bridge-server via its `well_known_uri` route (provisioned listener + discovery registration) and resolve the connector handler pin end-to-end
- [x] 7.10 Update proposal/design/spec deltas for the hardened semantics (per-grant tenant scoping, non-delegatable `DelegateGrants`, sender pinning, own-tenant identity checks, opt-in mTLS, disabled 0-RTT, bounded metadata); verify `openspec validate rebuild-guest-bridge --strict`
