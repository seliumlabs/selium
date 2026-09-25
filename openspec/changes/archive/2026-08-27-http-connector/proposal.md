# Proposal: HTTP Connector (First Edge Connector)

## Why

Guests can speak raw TCP once `guest-net-sockets` lands, but the target
developer experience is not "write an HTTP server from sockets" — it is
a typed handle: the guest sees `RpcConnection<HttpRequest, HttpResponse>`
and never touches a wire protocol. The connector model moves protocol
termination to the edge: a system guest (the connector) terminates TCP,
TLS, and HTTP/1.1, and forwards **typed, schema-encoded messages** over
ordinary shared-memory channels. App guests then need **zero `Network`
grants** — their entire attack surface is channel attach — which is the
strictest reading of non-negotiable 7 available in this architecture.

HTTP is the first connector because "a web server guest" is the
motivating use case, and because its mapping exercises the whole pattern
catalog: unary RPC for typical requests, server-streaming
(`streaming-rpc-patterns`) for chunked/SSE bodies.

## What Changes

- **`selium-proto-http`** (new crate): schema types (`HttpRequest`,
  `HttpResponse`, body chunks, trailers) via `selium-encoding`
  (`#[schema]`/`FlatMsg`). Protocol message types live in encoding
  crates, never in `selium-abi` — the ABI stays fenced.
- **`selium-connector-http`** (new system guest): a plain guest built on
  the raw socket SDK — no framework crate. It binds the public TCP
  listener, terminates TLS (rustls; certificates loaded via its storage
  grant), parses HTTP/1.1, and forwards typed requests to app guests.
- **Discovery-based routing**: app guests register served URI subtrees
  with discovery; the connector resolves Host + path → channel via
  discovery lookups (cached). No routing tables in the connector.
- **Per-connection channel pairs with tag correlation**: the accept
  model's per-connection regions carry typed frames; the connector keeps
  an in-flight map from protocol-native request ordering to frame tags.
- **Backpressure honesty**: a full ring stops socket reads at the edge
  (Park semantics) — the connector is never an unbounded buffer.
- **Trust boundary statement**: TLS terminates at the connector;
  plaintext crosses capability-gated channels only. The boundary is
  grant hygiene, not the wire (see design for the honest limits).

## Capabilities

### New Capabilities

- `http-connector`: edge termination of TCP/TLS/HTTP1.1 into typed
  channel-forwarded RPC; routing, correlation, backpressure, and the
  capability model for connector-served guests

### Modified Capabilities

(None — uses `guest-networking` sockets and `discovery-registration`
behaviour without changing their requirements.)

## Impact

- New crates: `selium-proto-http`, `selium-connector-http` (guest).
- Depends on: `guest-net-sockets` (raw sockets), `streaming-rpc-patterns`
  (streaming bodies/SSE), `network-capability-uris` (connector's bind
  grant scoped to `tcp://…:443`).
- The raw socket path remains fully public and unchanged: BYO-framework
  guests (hyper/axum) bypass the connector entirely.
- Golden path: `curl` → connector → typed channel → app guest →
  response + log line, in CI (invariant 6).
- Follow-ons (not in this change): DNS/WebSocket/IMAP/SMTP connectors;
  HTTP/2+H3 after the QUIC path is reinstated; the external `curl`-in-CI
  golden path once a live wasm guest deployment is available.
