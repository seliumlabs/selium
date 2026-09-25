# Design: Guest Network Sockets

## Context

Kernel proxies (`network_runtime.rs`) already pump bytes between OS
sockets and two-ring shared regions; the runtime dispatches the network
hostcalls with capability checks. The missing piece is purely the guest
SDK surface. The frozen `bridge` guest shows the intended consumer shape
(it references `selium_guest::UdpSocket::attach`, which does not exist).

## Goals / Non-Goals

**Goals:**

- Faithful `AsyncRead`/`AsyncWrite` guest streams (correct waker
  registration) so BYO-framework works
- Literals-only addressing enforced at the runtime boundary
- Binary datagram format ready for the future quinn adapter

**Non-Goals:**

- QUIC/`QuinnUdpSocket` reinstatement (frozen crates; built on this
  layer later)
- HTTP or other protocol overlays (see `http-connector`)
- URI-scoped grants (see `network-capability-uris`)
- Event-driven proxies (see `event-driven-net-proxies`); the existing
  polling proxies remain functional meanwhile

## Decisions

### Raw streams are the permanent public foundation

`TcpStream`/`UdpSocket` are public, first-class API — not an internal
layer for connectors. The typed-handle connector model
(`http-connector`) is additive on top. Rules:

- The raw stream implements `tokio::io::AsyncRead + AsyncWrite + Unpin`
  faithfully, including correct waker registration
  (`register_generation_wait` on the ring generation). This is what makes
  BYO-framework (hyper/axum) viable in a guest; it is a spec'd contract,
  not an accident.
- No overlay may wrap the stream exclusively. If a future HTTP helper
  consumes a `TcpStream`, raw access (`into_parts` or equivalent) must
  remain available.

### Literals-only addressing

`TcpConnect`, `TcpBind`, and `UdpBind` accept IP literals only; the
runtime validates by parsing `SocketAddr` and rejects names loudly
(`AbiErrorCode::MalformedPayload`). Rationale: host-side resolution is
ambient authority (a granted guest could trigger arbitrary DNS lookups,
and the grant would constrain a name whose resolution the guest cannot
see). Honest resolution is a typed RPC to the DNS connector. The guest
SDK validates early for ergonomics, but the runtime check is the
enforcement point.

### Binary datagram frames

```
[ver u8 = 1][family u8: 4|6][addr 4|16 bytes][port u16 LE][payload…]
```

Replaces the current string-prefixed format in the kernel UDP proxy.
Chosen now, while the UDP path is young, because quinn's
`Transmit`/`RecvMeta` carry binary `SocketAddr` — the frozen
`QuinnUdpSocket` adapter becomes a pure codec, and we avoid a flag day
when `quinn-guest-integration` is reinstated.

### Region layout reuse

Guest attach reuses the existing pieces: `SharedRegionDescriptor` →
attach hostcall → `MultiMemoryHeader::parse` → sub-region 0 = inbound
(`Reader`), sub-region 1 = outbound (`Writer`). No new region kinds.

## Risks / Trade-offs

- **Half-close semantics**: the kernel proxy maps guest
  `writer_count == 0` to `shutdown(Write)`; the guest SDK must not
  decrement writer count on a mere `poll_shutdown` unless the stream is
  actually closing. Covered by a spec scenario.
- **Frame boundaries are not message boundaries** on TCP (8 KiB host
  read chunks). `Reader` presents a byte stream; the spec forbids
  assuming frame == message.
