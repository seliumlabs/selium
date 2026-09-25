# Proposal: Guest Network Sockets (Tier 0)

## Why

The host side of guest networking already landed in the spine: the ABI
carries `TcpBind`/`TcpConnect`/`UdpBind` hostcalls, the kernel proxies OS
sockets to shared-memory ring pairs, and the runtime enforces
`Capability::Network` per hostcall. What does not exist is the guest side:
`selium-guest` has no `net` module, so no WASM guest can actually use any
of it. A web server guest is impossible today not because the substrate is
missing, but because the SDK surface is.

This change delivers the smallest honest guest networking layer: raw TCP
and UDP sockets as channel overlays, exactly per non-negotiable 6. It is
deliberately the *foundation* tier: connector guests (see `http-connector`,
`dns-connector`) are built from these primitives, and the raw stream stays
public forever as the escape hatch for users bringing their own frameworks
(e.g. hyper/axum via `hyper_util::rt::TokioIo`).

## What Changes

- **`selium-guest::net` module** (new):
  - `TcpStream` — `connect(addr)` via `TcpConnect` hostcall, plus
    `Accept` impl building a stream from an `IncomingConnection`.
    Implements `tokio::io::AsyncRead + AsyncWrite` by delegating to
    `Reader`/`Writer` over the two-ring region, with waker registration
    via `register_generation_wait` (no spin, no lost wake).
  - `TcpListener` — `bind(addr)` via `TcpBind`, wrapping the returned
    host queue in `ResourceListener::from_queue`; `accept()` yields
    `TcpStream`.
  - `UdpSocket` — `bind(addr)` via `UdpBind`; `poll_send(Datagram)` /
    `poll_recv() -> Datagram` where `Datagram { addr: SocketAddr, payload }`.
- **Literals-only `TcpConnect`/`TcpBind`/`UdpBind`**: the runtime rejects
  any address that is not an IP literal (`SocketAddr` parse). Name
  resolution is never performed ambiently by the host; resolution is a
  capability-gated typed RPC via the DNS connector (specced separately in
  `dns-connector`). Immediate testing uses `127.0.0.1`.
- **Binary datagram frame format** (breaking, kernel + spec): replace the
  string-addressed datagram frame (`[u16 len]["ip:port"][payload]`) with
  a binary frame (`[ver u8][addr 4|16 bytes][port u16 LE][payload]`).
  Motivation: no per-datagram string parse/alloc in the kernel hot path,
  and a 1:1 mapping onto `quinn` `Transmit`/`RecvMeta` when the frozen
  QUIC crates are reinstated on top of this layer.

## Capabilities

### New Capabilities

(None — this change exposes existing kernel/ABI behaviour through the
guest SDK.)

### Modified Capabilities

- `guest-networking`: adds guest `TcpListener`/`UdpSocket` APIs,
  literals-only addressing, and the raw-stream escape-hatch contract
- `udp-transport`: binary datagram frame format (**BREAKING** for the
  kernel↔guest datagram codec)

Grant model note: uses existing `Capability::Network` with
`ResourceClass::TcpListener`/`TcpStream`/`UdpSocket` grants. URI-scoped
grants arrive separately in `network-capability-uris`; this change works
with class-only grants.

## Impact

- `selium-guest`: new `net` module; no breaking changes to existing API.
- `selium-kernel`: datagram frame codec changes (binary); literals-only
  validation for network bind/connect entry points.
- Specs: MODIFIED `guest-networking`, MODIFIED `udp-transport`.
- Golden path: new CI test — WASM guest TCP echo server, host-side
  client, log line (`cargo test -p selium-runtime`), per invariant 6.
