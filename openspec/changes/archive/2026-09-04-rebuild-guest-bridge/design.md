## Context

The bridge guest at `guests/bridge/` is frozen: excluded from the workspace, and dependent on the deleted `selium-quic` and on `selium-guest` QUIC APIs that no longer exist (`QuinnUdpSocket`, `SeliumQuinnRuntime`, `UdpSocket::attach`). The live `quic-connector` owns the reusable machinery: `build_endpoint` (quinn over the shm `QuicUdpSocket` + `ConnectorRuntime`), SNI→discovery routing with cache/evict, per-stream two-ring byte channels delivered via `ResourceSender`, and byte-relay pumps with FIN/EOF fidelity and backpressure.

Two relevant seams already exist. First, `selium-wire`'s `FramedRead`/`FramedWrite` operate over `MessageTransport = AsyncRead + AsyncWrite + Unpin`, so the deleted `QuicTransport` is unnecessary — any relayed byte channel wraps directly in the framing codec. Second, the runtime already enforces grant monotonicity on spawn: `validate_child_grants` requires every child grant to be covered by a parent grant (`parent_grant_covers_child`), and children inherit the parent's tenant.

## Goals / Non-Goals

**Goals:**
- Decompose the bridge into a per-tenant `bridge-server` (acceptor, identity→grants, spawn) and per-stream `bridge-channel` (attach + splice), reusing the connector for all QUIC termination.
- Route by TLS identity per connection and by discovery URI per stream, superseding SNI→guest routing for bridge traffic.
- Attribute authority: each `bridge-channel` holds exactly the authenticated client's grants; capability enforcement stays in the runtime.

**Non-Goals:**
- Discovery URI taxonomy (`sel://tenant/project/env/...`, wildcards, multi-URI semantics, protocol disable via URI) — its own change.
- An accountant/limits guest, a tenant control-plane guest (bridge-server spawn timing is assumed to exist), and WASM integrity checksums.
- A multi-tenant `bridge-server`; tenants map one-to-one to bridge-servers.

## Decisions

**D1 — Routing inversion (identity per connection, URI per stream).**
The connector routes a connection to a tenant's `bridge-server` by the presented SNI (`sel-quic://<tenant>/bridge`); the authenticated TLS identity names the client, and each stream is then bound to a channel URI via the typed handshake. Alternative considered: keep SNI→serving-guest and attach identity at the guest — rejected because it gives no per-client authority boundary (the edge is shared and impersonates no one).

**D2 — Per-stream `bridge-channel` processes (not per-client).**
One process per QUIC stream = one channel membership; channel multiplexing moves into the external client library. Rejected alternative: per-client process (the old `guest-bridge` model) — requires an identity→process registry and a connection-close signal from the connector just to match streams to sessions, and turns the bridge into a channel manager. Costs accepted: spawn amplification (see D11), no single process representing "the client" for session-scoped policy (deferred to the future accountant/discovery), and metering attribution must ride a spawn label.

**D3 — `bridge-server` is a connector-served guest, not a QUIC server.**
quinn cannot transfer a live connection between two wasm processes, so per-user endpoints are impossible without host-side UDP connection demux (rejected: a runtime modification, which the existing spec forbids). The connector keeps the endpoint; `bridge-server` receives per-stream handoffs and never relays bytes.

**D4 — `shared_id` passes as an entrypoint argument; no re-send.**
`bridge-server` spawns `bridge-channel(shared_id, grants)` where the region is delivered via the connector's queue handoff (ownership shared at receive) and conferred by an `ExplicitResource` grant at spawn. No second queue handoff exists in the data path.

**D5 — mTLS identity = `{ tenant_scope, leaf SPKI fingerprint }`.**
Per-tenant trust anchors give environment/project isolation: the anchor that verifies the client chain determines the tenant scope. The leaf's public key (SPKI) is fingerprinted rather than the certificate DER, so "same key = same client" survives certificate renewal. Rejected: hashing the full chain (`Vec<hash>`) — fragile to intermediate rotation and redundant with the trust anchor.

**D6 — Generic handoff metadata (one ABI change for identity).**
`HostQueueSend`/`HostQueueRecv` gain an opaque metadata payload surfaced as `IncomingConnection.metadata`. The receiver cannot otherwise know who the sender is (the runtime auto-populates only `client_process_id`). TLS identity is the first user; the HTTP connector can reuse it later.

**D7 — `DelegateGrants` capability for delegation.**
Keying delegation off ownership is rejected: it only covers resource grants (`ExplicitResource` for regions the delegator owns), not arbitrary client grants such as `Network` or `Storage`. The subset rule stays the default; `DelegateGrants` scoped to a tenant authorizes `bridge-server` to confer child grants within that tenant, running well-formedness admission still. Two hardening rules close review findings: (a) delegation admits only child grants that **each** carry an in-scope tenant selector — a selector-less grant is unrestricted within its capability and would escape the tenant fence, so it falls through to the subset check; (b) `DelegateGrants` can never be conferred on a child (bootstrap-provisioned only), so the exception to authority monotonicity cannot chain through spawned processes. This is a deliberate, tenant-fenced exception to authority monotonicity (see Risks).

**D8 — Typed handshake/termination frames.**
The first frame on each stream is a typed message naming the channel URI; failures are a typed termination frame followed by stream close. Typed frames (rkyv/FlatBuffers) keep the control plane uniform with "typed end-to-end" instead of bare strings, and a reserved flags bit or tag band distinguishes control from relayed data if needed.

**D8a — Sender pinning for handoff metadata (review hardening).**
Handoff metadata is sender-controlled and opaque, and the bridge route's queue is resolvable via discovery by any `HostQueue`-capable guest — so an unpinned bridge-server would let any guest forge an authenticated identity and mint grants. Fix: serve-side listeners pin the registered protocol handler for their scheme (`ResolveProtocolHandler` hostcall; bootstrap-authoritative, unforgeable) as the only accepted sender; `QuicServe`/`HttpServe`/`HttpStreamServe::bind` pin automatically, and the bridge-server pins its provisioned listener. Mismatched handoffs are refused by attach-then-close (the sender observes EOF). Fails closed: no registered handler → no serving. Tenant-scoped discovery resolves remain out of scope; the pin is the interim defence.

**D8b — Identity tenant must match the server's own (review hardening).**
The connector derives the identity's tenant from the verifying anchor, but SNI routing is independent of it: a client verified by another tenant's anchor can present a bridge SNI. The bridge-server queries its own tenant via `SelfInfo` and refuses identities whose tenant differs from its own.

**D8c — mTLS is opt-in; 0-RTT disabled (review hardening).**
With no anchors configured the connector serves without client authentication (restoring pre-mTLS deployments and the spine test); with anchors configured, client auth is mandatory endpoint-wide. Configured-but-broken anchors fail loudly rather than silently downgrading. TLS 1.3 0-RTT early data stays disabled (`max_early_data_size = 0`): early data is replayable and the connector relays stream bytes into the fabric under the authenticated identity.

**D8d — One-writer ring membership for fabric-close detection (review fix).**
The original pipe built two full `ShmTransport`s on the fabric ring (two counting writers), so `writer_count == 0` — the ring's EOF signal — could never fire while the pipe lived, and the "fabric closes → finish stream" scenario was undetectable by construction. Fix: split ring adapters (read-only + write-only) so the pipe contributes exactly one counting writer and one blocking reader. Fabric-close detection = no pending data AND only the pipe's own members remain (`writer_count == 1 && reader_count == 1`). Count-based liveness cannot distinguish "all inner peers left" from "none ever attached": a pipe bridging a memberless fabric finishes the client's stream (a clean FIN) rather than parking forever; non-blocking reader-only peers are invisible to the reader count (best effort).

**D9 — Either-half-closes teardown.**
The connector's `join!(both)` semantics are insufficient: a dead client leaving a quiet fabric channel means the read pump parks in `BufferEmpty` forever. The `bridge-channel` ties both pump lifetimes together — either end closing tears down the whole pipe — so self-cleaning is complete, not mostly-complete.

**D10 — Refuse unknown identity by attach-then-close.**
Silently dropping a delivered stream stalls the client (the connector's pump parks on a region nobody attaches). Refusal therefore attaches the region and immediately closes it, so the connector observes EOF and FINs the client's stream.

**D11 — Bounded spawn at `bridge-server`.**
Per-stream spawns amplify DoS: a connection can mint cheap streams that each cost a process. The server enforces a spawn bound (per identity/tenant; exact policy is an open question) and refuses overflow. Slots acquired for spawns that fail are returned, so failed spawns do not permanently consume a client's budget.

## Risks / Trade-offs

- **[DelegateGrants breaks authority monotonicity]** → Tenant-scoped, held only by system guests, with content dictated by the identity guest. Named as a deliberate, explicit capability rather than an ambient privilege.
- **[mTLS is endpoint-global when enabled]** → Opt-in (D8c): anchors configured ⇒ mandatory client auth on every route; no anchors ⇒ none. Bridge routes must only be deployed on anchor-configured connectors (the bridge-server attributes authority from handoff identity metadata). Per-route/per-SNI auth policy remains an open question. Wasm checksums protect code integrity, not policy correctness, so the identity guest remains the trust boundary regardless.
- **[Forged handoff metadata]** → Sender pinning (D8a): handoff metadata is sender-controlled; serve-side listeners refuse handoffs from any process other than the scheme's registered handler. Tenant-scoped discovery resolves are the eventual companion fix (out of scope).
- **[Spawn amplification]** → Bounded spawn (D11) plus scheduler backpressure; measure before optimizing.
- **[Region lifetime across three processes]** → The region's peers are only the connector and the bridge-channel; `bridge-server` never maps it. Ownership accumulates on handoff and regions are reclaimed at pipe teardown.
- **[Zombie pipes on quiet fabric channels]** → Either-half teardown (D9) plus one-writer membership (D8d) prevents lingering processes.

## Migration Plan

1. `selium-abi`: add metadata to `HostQueueSend`/`HostQueueRecv`, add `Capability::DelegateGrants` (coordinate host/guest builds — rkyv enum changes are not mixed-version safe).
2. `selium-runtime`: branch in `validate_child_grants` for `DelegateGrants`; surface handoff metadata in `HostQueueRecv`.
3. `connector-quic`: client verifier from configured per-tenant anchors, identity extraction (`sni_of`-style), attach identity to each handoff, refuse untrusted connections.
4. Guests: reactivate `guests/bridge` as `bridge-server` + `bridge-channel`.
5. Rollback: ABI additions are additive per direction but not mixed-version safe; deploy host and guests together.

## Open Questions

- **mTLS scope**: dedicated bridge connector endpoint vs global client-auth with per-route policy. Deployment nuance; does not change the bridge specs. (Decided meanwhile: mTLS itself is opt-in — see D8c.)
- **Identity source stub shape**: `HashMap<fingerprint, grants>` is the interim; the identity guest's RPC surface is deferred. The stub is keyed by fingerprint and consulted only after the identity's tenant matches the server's own (D8b).
- **Spawn bound policy**: exact concurrency/rate values (absolute N vs per-identity vs per-tenant). The implemented bound is a per-identity lifetime cap with slots released on failed spawns; true concurrency tracking waits on a child-exit signal from the supervisor.
- **Fabric liveness signal**: writer/reader counts approximate "all inner peers gone" (D8d); an explicit fabric-close marker in the ring layout would remove the memberless-fabric ambiguity.
