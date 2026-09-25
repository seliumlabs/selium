# Design: QUIC Connector

## Context

See proposal.md for motivation. Three building blocks exist and shape the
approach:

- **Connector substrate** (`connector-http`): a system guest binds a
  listener, resolves routes via discovery, and hands each accepted unit of
  work to an app guest over a shared-memory channel. The per-connection
  handoff is `ResourceSender::attach(target.resource_id)` → allocate a
  two-ring region → `sender.send(shared_id)`; the app guest drains its
  `ResourceListener` and attaches the region (`selium-shm` layout: one
  parent region, two sub-memories, `MultiMemoryHeader`).
- **Byte-stream abstraction** (`selium-guest::net::tcp::TcpStream`):
  attaches a two-ring region and presents `AsyncRead`/`AsyncWrite` by
  prepending/stripping `FrameHeader`s. This is exactly the shape a
  "relayed byte channel" needs and is reusable for QUIC streams.
- **Legacy QUIC integration** (frozen `crates/quic` + `guests/bridge`):
  proved quinn runs on wasm32 via `Endpoint::new_with_abstract_socket`
  with a custom `AsyncUdpSocket` over shm datagrams plus a custom
  `Runtime` (spawn/sleep/now over guest executor and hostcalls). It is
  superseded by this change; its crates are not in the active workspace.

The one genuinely new problem is *which* guest a QUIC connection belongs
to: QUIC has no Host header. The answer (below) is SNI.

## Goals / Non-Goals

**Goals:**

- A payload-agnostic, byte-transport QUIC edge: connector relays stream
  bytes verbatim; users frame them with their own FlatBuffers schemas.
- App guests serve QUIC with zero `Network` grants and zero quinn
  dependency.
- Reuse the existing channel/accept substrate (exactly one new framing
  seam, not a parallel one).

**Non-Goals:**

- Datagrams and unidirectional-stream relay (v1 is bidirectional streams).
- HTTP/3 (`h3`/QPACK/RFC 9114) — it layers on this transport later.
- A shared connector framework crate (rule of three: still one connector).
- End-to-end QUIC passthrough to app guests (they never see quinn).

## Decisions

### Route on SNI, once per connection

The TLS 1.3 ClientHello carries the server name (SNI); quinn exposes it
via `Connection::handshake_data()`. The connector normalises SNI →
`sel-quic://<name>` and resolves it through discovery exactly like the
HTTP connector resolves Host+path, caching the result. Every stream on
the connection goes to that one resolved guest, so resolution happens once
per handshake, not per stream. Unknown/absent SNI → the handshake is
refused and no app guest is contacted.

*Alternatives considered:* a fixed single well-known endpoint (rejected —
no multi-tenant routing); ALPN as the route key (rejected — ALPN is a
superset selector for the *application protocol*, and several protocols
can legitimately share one server name; SNI is the addressing signal);
per-stream discovery (rejected — needless lookups; stream scope is
derived from the connection).

### No `proto-quic` crate — relay bytes, not typed messages

Unlike HTTP/DNS, QUIC payloads are not platform-defined. Adding wire types
would invent a schema users immediately replace. The connector therefore
uses the *raw channel layer* (`selium-shm` rings) rather than typed RPC,
and the guest API exposes byte streams. Users apply `selium-encoding`
schemas over those streams.

*Alternatives considered:* a generic "frame" proto type (rejected —
re-encodes bytes the connector never needs to interpret; adds a copy and a
schema authority for no benefit).

### Per-stream channel = a two-ring region, byte-stream semantics

Each accepted bidirectional stream becomes one two-ring region (the same
`MultiMemoryHeader` layout `TcpStream` already consumes), granted
`ExplicitResource` to {connector, app guest}, and delivered via
`ResourceSender` to the resolved guest's listener queue. The connector and
the guest both attach it as a byte stream (`AsyncRead`/`AsyncWrite`),
mirroring `TcpStream::attach_region`.

To avoid duplicating the header/frame logic, the existing
`TcpStream::attach_region` implementation is factored into a shared
internal helper (in `selium-guest`) used by both `TcpStream` and the new
`QuicStream`, so TCP and QUIC byte channels share one layout and one
implementation.

*Alternatives considered:* nested per-connection control channel that
announces streams (rejected — QUIC already multiplexes; re-multiplexing a
second level buys nothing); reusing `selium_shm::transport::ShmTransport`
+ `FramedRead/FramedWrite` (rejected — that surfaces `selium-wire` *frame*
semantics to guests, not a byte stream; bytes are the contract).

### Quinn lives entirely in the connector guest

`quinn` is added to `[workspace.dependencies]` with
`default-features = false` and `features = ["rustls-ring"]` (no
`runtime-tokio`). The connector guest supplies both halves quinn needs:
an `AsyncUdpSocket` adapter wrapping `selium_guest::net::UdpSocket`
(`Transmit` ↔ `Datagram`) and a `quinn::Runtime` over the guest executor,
`sleep` hostcall, and clock hostcalls. The endpoint is built with
`Endpoint::new_with_abstract_socket`. No quinn types enter `selium-guest`,
so app guests stay quinn-free.

*Rationale vs. the legacy layout:* the old design put quinn adapters in
`selium-guest` (behind a `quinn` feature) because *bridge* guests ran
quinn themselves. In the connector model only the connector runs quinn,
so the adapter belongs to the one crate that needs it — narrower SDK
surface, no feature flag, no quinn in every guest build.

### TLS and RNG/time re-use the HTTP connector's pattern

Server certificate + key load from blob storage via the connectors's
`Storage` grant, parsed with `rustls-pemfile`; loud failure on missing or
invalid material. The wasm32 `getrandom` backend and `web-time` time
source are registered exactly as in `connector-http` before any TLS
operation. ALPN is a configurable list (manifest), selectable but opaque
to forwarding — the connector never interprets the negotiated protocol,
because payload is the user's business.

### Backpressure honesty via quinn flow control + Park

Two directions, both already bounded by the substrate:

- **Client → guest:** when the stream's outbound ring is full, the
  connector simply stops polling the quinn `RecvStream`; quinn's receive
  flow control stops the peer. No connector-side buffer.
- **Guest → client:** the guest writes into the ring; a slow client makes
  the connector's ring *read* stall, which parks the guest's ring *write*
  (Park semantics) — throttling at the producer, not at a connector
  buffer.

No path buffers without bound; the connector never accumulates stream
bytes.

### Lifecycle and delivery

Stream FIN (client done) → connector finishes writing the ring and closes
the channel, surfacing EOF to the guest. Guest closing the channel →
connector calls `SendStream::finish()` (or `reset()` on error/half-close
violation). Because each stream is an independent channel, there is **no
ordering-map equivalent of the HTTP connector's `CorrelationMap`** — QUIC
stream ordering is preserved by the transport itself, and the connector
relays each stream independently.

### Delete `crates/quic`

`selium-quic`'s `QuicTransport` has no active caller: the active workspace
never lists the crate, and `guests/bridge` (frozen) references a
`core/quic` path that is equally dormant. QUIC stream handling is now
internal to the connector, so the crate is deleted rather than migrated.
The frozen `guests/bridge` is left untouched (it cannot build today
either way); it is re-derived from this connector if ever re-activated, as
the `guest-bridge` spec delta records.

## Risks / Trade-offs

- **quinn on wasm32 is untested in this workspace (the legacy proof is
  frozen).** → The connector task list carries a spike first: get the
  endpoint to complete a handshake against a host-side quinn client before
  any relay work. ECN and segmentation are degraded (`RecvMeta.ecn` set to
  `Ect0`; no GSO over shm frames) — acceptable for v1, noted for the
  external client SDK follow-on.
- **SNI is not authenticated by itself.** It is a routing hint, and MXS
  integrates SNI+ALPN with cert validation as usual. The connector trusts
  that quinn/vendor stack; misrouting risk is equivalent to the HTTP
  connector's Host-header trust.
- **One connector process is a single throat for all QUIC traffic.** →
  Same mitigation as `connector-http`: a plain guest, restartable by the
  supervisor, scoped per listener, failures isolated per failing session.
- **Channel allocation per stream is churnier than per connection.** →
  Reuses the same allocator/free path as `rpc::connect`; per-stream regions
  are the security boundary the capability model needs (`ExplicitResource`
  per stream).
- **Frozen `guests/bridge` becomes conceptually stranded.** → Already
  non-building/out-of-workspace; spec delta records the supersession.

## Migration Plan

Additive except for the deletion: `selium-quic` is removed together with
its (nonexistent in the active workspace) references. The raw
`UdpSocket`/`TcpSocket` paths are untouched, so BYO-framework guests are
unaffected. Rollback is "revert the change"; no data migration applies.

## Open Questions

- External host-side client SDK (running quinn on a non-WASM host against
  the connector) — deferred; does not change specs or tasks here.
- Default listener address/port and default ALPN list — policy to confirm
  at implementation time from provisioning conventions; recorded in the
  connector's config, not in spec behavior.
