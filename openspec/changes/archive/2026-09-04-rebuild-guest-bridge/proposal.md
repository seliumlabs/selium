## Why

The bridge guest at `guests/bridge/` is frozen: it is excluded from the workspace and depends on the deleted `selium-quic` crate and removed `selium-guest` QUIC APIs (`QuinnUdpSocket`, `SeliumQuinnRuntime`, `UdpSocket::attach`). The live `quic-connector` already terminates QUIC over shared-memory UDP and relays byte streams — but it routes by SNI to one serving guest with **no client authentication**, so an external application cannot join the fabric as an authenticated remote participant subject to the same capability checks as internal guests. This change rebuilds the bridge on connector machinery: a per-tenant `bridge-server` authenticates external clients by TLS identity and spawns per-stream `bridge-channel` processes that hold the client's grants.

## What Changes

- **Two new system guest roles** replace the frozen single bridge guest:
  - `bridge-server` (one per tenant): a connector-served guest that receives stream handoffs tagged with a TLS identity, maps identity → capability grants, and spawns a `bridge-channel` per stream.
  - `bridge-channel` (one per QUIC stream): attaches the relayed region, accepts a typed handshake frame naming a channel URI, resolves it via discovery, attaches the fabric ring under the client's grants, and splices tagged frames between the stream and the ring.
- **Routing inversion**: a QUIC connection is routed by TLS identity to a per-tenant `bridge-server`; each stream is then bound by URI to a fabric channel. This replaces the connector's SNI→serving-guest routing for bridge traffic.
- **Opt-in mTLS client authentication in `connector-quic`**: per-tenant CA trust anchors; client identity is `{ tenant_scope, leaf SPKI fingerprint }`. With anchors configured, the connector verifies key possession during the handshake, refuses untrusted clients before any guest contact, and passes the identity along with each stream handoff. With no anchors configured it serves without client authentication (bridge routes must only be deployed on anchor-configured connectors). TLS 1.3 0-RTT early data stays disabled: early data is replayable, and the connector relays stream bytes into the fabric under the authenticated identity.
- **Generic handoff metadata (BREAKING — ABI)**: `HostQueueSend` gains an opaque, size-bounded (4 KiB) metadata payload surfaced to the receiver as `IncomingConnection.metadata`, so edges can attach peer identity to a handoff. TLS identity is the first user of it. Handoff metadata is sender-controlled, so serve-side listeners pin the registered protocol handler (the connector) as the only accepted sender; handoffs from any other process are refused.
- **`DelegateGrants` capability**: lets a scoped system guest confer child grants it does not itself hold, provided every child grant carries a tenant selector within the delegation scope. `DelegateGrants` itself can never be conferred on a child (bootstrap-provisioned only), so the exception to authority monotonicity cannot chain. The existing "child grants must be a subset of parent grants" rule remains the default.
- **Typed per-stream handshake/termination frames** layered on the existing `selium-wire` codec (encoded before transiting QUIC). The bridge relays tagged frames verbatim and does not parse application payloads.
- The stale `guest-bridge` spec is replaced rather than patched incrementally.

## Capabilities

### New Capabilities

None — this change decomposes and rebuilds the existing `guest-bridge` capability in place.

### Modified Capabilities

- `guest-bridge`: rewritten from a single per-user frame-relay guest (on the deleted `selium-quic`) to the `bridge-server`/`bridge-channel` model with TLS-identity authentication, sender pinning, own-tenant identity checks, and per-stream channel binding.
- `quic-connector`: adds opt-in mTLS client authentication (per-tenant trust anchors), TLS-identity propagation on stream handoff, and disabled 0-RTT.
- `capability-enforcement`: adds the `DelegateGrants` capability with tenant-scoped, per-grant delegation semantics; `DelegateGrants` is never conferable on children.
- `resource-handshake`: extends the handoff contract so a `HostQueueSend` carries a bounded opaque metadata payload surfaced in `IncomingConnection`, plus hostcalls for a guest's own identity (`SelfInfo`) and for resolving the registered protocol handler of a scheme.

## Impact

- **Guests**: `guests/bridge/` reactivated and rebuilt as `bridge-server` + `bridge-channel` (or two new sibling crates); `guests/connector-quic/` gains a client verifier, identity extraction (`sni_of`-style), and metadata-bearing delivery. All connector-served `*Serve` types (`QuicServe`, `HttpServe`, `HttpStreamServe`) pin their listener to the scheme's registered handler, refusing handoffs from any other process.
- **`selium-abi`**: `HostQueueSend` gains a bounded metadata field (surfaced via the recv output's `ConnectionInfo`); `Capability` gains `DelegateGrants`; new `SelfInfo` and `ResolveProtocolHandler` hostcalls; all are wire-format changes (BREAKING for mixed-version hosts/guests).
- **`selium-runtime`**: `validate_child_grants` gains a delegation branch (skip the subset check only when the parent holds a tenant-matching `DelegateGrants` **and every child grant carries an in-scope tenant selector**, still running well-formedness admission; `DelegateGrants` conferment is denied outright); `HostQueueSend` metadata is size-capped and surfaced on recv; the new hostcalls are dispatched.
- **`selium-wire`**: no change expected — the framed codec (`FramedRead`/`FramedWrite` over `AsyncRead + AsyncWrite + Unpin`) already fits the relayed byte channel.
- **Out of scope** (deliberate): discovery URI taxonomy (`sel://tenant/project/env/...`, wildcard queries, protocol disable via URI), tenant-scoped discovery resolves (the bridge-server's sender pin is the interim defence), a tenant control-plane guest (bridge-server spawn lifecycle is assumed to exist), an accountant/metering-limits guest, and WASM integrity checksums.
