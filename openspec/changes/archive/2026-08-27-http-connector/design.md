# Design: HTTP Connector

## Context

Three-tier layering from the networking exploration: Tier 0 raw sockets
(`guest-net-sockets`), Tier 1 connectors (this change), Tier 2 the
frozen QUIC bridge for Selium-native external clients. The connector is
the *terminating* edge; the bridge is the *transparent* edge — siblings,
not rivals. See proposal.md for motivation.

## Goals / Non-Goals

**Goals:**

- App guests serve HTTP with zero `Network` grants
- One external wire encoding: real HTTP/1.1 (browsers and `curl` work
  on day one)
- Typed schema messages inside the fabric

**Non-Goals:**

- A connector framework crate (plain guest + codec; see Decisions)
- HTTP/2 / H3 (needs QUIC; the typed API is transport-agnostic so they
  slot in later)
- End-to-end TLS passthrough to app guests (that *is* the raw path;
  noted as the supported alternative)

## Decisions

### No framework crate — routing decomposes into discovery + a map

Connector routing is two mechanisms, both already idiomatic:

1. **Ingress routing = discovery.** App guests register
   `sel://<tenant>/...` subtrees with the discovery guest; the connector
   resolves Host + path to a channel and caches the lookup. The
   connector holds no routing table; it is a stateless router.
2. **Reply correlation = an in-flight map.** Protocol-native request
   identity (HTTP/1.1 per-connection ordering; later H2 stream IDs) maps
   to the frame tag, which `transport-abstraction` guarantees
   end-to-end. Per-connection channel pairs collapse most correlation to
   "reply on the same channel with the same tag".

The glue is genuinely protocol-specific (H2 stream IDs ≠ DNS txids), so
a shared framework would leak. Rule of three: extract common helpers
only when the third connector duplicates them.

### Typed handles via patterns, not a bespoke HTTP server API

The app-guest surface is `RpcConnection<HttpRequest, HttpResponse, M>`
(unary) and server-streaming for chunked/SSE bodies — the standard
`selium-wire` patterns over a channel transport. "Web server guest" =
"a guest serving a URI subtree via typed RPC".

### TLS terminates at the connector — honestly stated

- The connector holds the `Network` + `UriPrefix("tcp://…:443")` bind
  grant and a `Storage` grant for certificates.
- Plaintext crosses only capability-gated channels. Per-connection
  regions are granted with `ExplicitResource` to exactly {connector, app
  guest}; broad `UriPrefix` shared-memory grants would widen exposure
  and are called out in the spec as an anti-pattern.
- **Limits, stated plainly**: the host is TCB (it can read every ring —
  hypervisor-equivalent trust); and region reuse is a leak vector until
  allocator zeroing lands — tracked as a dependency on
  `hardened-consumer-shared-memory`. Cross-guest interception is
  prevented *by the capability system*, not by encryption.

### Backpressure honesty

Each connection runs a windowed pipeline: parsed requests are forwarded
concurrently up to a bounded in-flight window; when the window is full
the connector stops reading from the client socket until a forward
completes (TCP flow control propagates through the edge). Replies are
reordered into request order by a correlation buffer whose queues are
all bounded, so no path buffers without bound. Streaming bodies use
server-streaming RPC so the edge never buffers a whole body; slow
consumers park ring writers at the transport layer (Park semantics).

## Risks / Trade-offs

- Connector is a single point of protocol complexity → it is a plain
  guest: restartable by the supervisor, scoped per listener, failures
  isolated per failure-isolation norms.
- Discovery cache staleness → registrations are live-table entries;
  connector invalidates on change notification, and a stale attach fails
  loudly (grant miss) rather than misrouting.
- Header/body size limits are policy at the edge → connector enforces
  explicit limits; oversized requests get typed error responses.

## Migration Plan

None — additive. The raw socket path (option 1) ships first and remains
the fallback while the connector proves out.
