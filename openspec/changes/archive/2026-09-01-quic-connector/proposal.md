# Proposal: QUIC Connector (Byte-Transport Edge Connector)

## Why

The connector pattern now serves HTTP and DNS by terminating a protocol
at the edge and forwarding typed messages over shared-memory channels.
QUIC is the missing transport-tier connector: a low-latency, multiplexed
byte transport that lets the platform speak to modern clients and peers
without every app guest re-implementing a QUIC stack. This change moves
QUIC termination to the edge — a system guest runs quinn and relays raw
stream bytes over shared-memory channels — so application guests serve
QUIC traffic with **zero `Network` grants** and no quinn dependency of
their own.

Payload is deliberately **not** a platform-defined schema. QUIC here is a
byte transport: end users decide the wire format and bring their own
FlatBuffers schemas (`selium-encoding`) on top of the relayed byte
streams, exactly as raw-TCP guests choose their own framing today.

## What Changes

- **`selium-connector-quic`** (new system guest): a plain guest built on
  the raw socket SDK that runs a quinn `Endpoint` over the guest
  `UdpSocket`, terminates QUIC (TLS 1.3; certificates loaded via its
  storage grant), and relays each accepted bidirectional stream's bytes
  between the wire and a per-stream shared-memory channel. The connector
  never parses or validates application payloads.
- **SNI-based discovery routing**: the QUIC handshake's server name (SNI)
  is the route key. The connector resolves `sel-quic://<name>` via
  discovery (cached) and hands every stream on that connection to the
  resolved guest. Unknown names are refused at the handshake — no app
  guest is ever contacted. No routing tables in the connector.
- **Per-stream byte channels with tag correlation**: each QUIC stream
  becomes an isolated shared-memory byte channel granted with
  `ExplicitResource` to exactly {connector, app guest}. Stream ordering
  and FIN/RESET lifecycle are preserved end-to-end.
- **Backpressure honesty**: a full ring parks the connector's read of
  that stream (quinn flow control pushes back to the peer); slow clients
  park ring writers before the guest. The connector is never an unbounded
  buffer.
- **App-guest byte-transport API**: `selium-guest::net::quic` — bind a
  `sel-quic://` URI, accept per-stream byte channels (read/write halves),
  and frame them with any user schema. No quinn dependency on the guest
  side.
- **Delete `crates/quic` (`selium-quic`)** **BREAKING**: the frozen
  `QuicTransport`/`MessageTransport` crate is removed. It has no active
  callers (the active workspace never listed it); QUIC stream handling is
  now internal to the connector guest, and quinn's adapter types
  (`AsyncUdpSocket` + `Runtime` impls) live inside the connector crate,
  not `selium-guest`.

## Capabilities

### New Capabilities

- `quic-connector`: edge termination of QUIC (TLS 1.3, quinn) into
  shared-memory byte-stream forwarding; SNI routing, per-stream channel
  isolation, backpressure, stream lifecycle fidelity, and the
  zero-`Network`-grant capability model for connector-served QUIC guests.

### Modified Capabilities

- `quinn-transport`: the stream-level `QuicTransport` in `selium-quic`
  and the `selium-quic` crate are removed; QUIC is terminated and relayed
  by the connector instead, and quinn's UDP/runtime adapters live in the
  connector guest rather than `selium-guest`.
- `guest-bridge`: the frozen bridge guest's `selium-quic` dependency is
  removed (the crate it names no longer exists); the bridge's role is
  superseded by the QUIC connector and will be re-derived from it if ever
  re-activated.

## Impact

- New crates: `selium-connector-quic` (guest). Deleted crate:
  `selium-quic` (`crates/quic`).
- `selium-guest` gains a `net::quic` serve module (byte-transport API +
  `sel-quic` scheme + `QUIC_STREAM` interface marker) with no quinn
  dependency.
- `quinn` is added to `[workspace.dependencies]` with
  `default-features = false` + `rustls-ring`; the connector supplier
  supplies its own `quinn::Runtime` and `AsyncUdpSocket` adapter (reusing
  the getrandom/`web-time` hostcall pattern from `connector-http`).
- Depends on: `guest-net-sockets` (raw sockets), `transport-abstraction`
  (channel/tag semantics), the connector runtime/accept substrate already
  exercised by `connector-http`, and `discovery-registration` behaviour
  (unchanged).
- App guests serve QUIC traffic with no `Network` grants — only
  per-stream `ExplicitResource` channel attach grants, mirroring the HTTP
  connector's capability model.
- The raw `UdpSocket`/`TcpSocket` path stays fully public and unchanged:
  BYO-framework guests keep their own networking.
- Follow-ons (not in this change): datagram relay and unidirectional
  stream relay; HTTP/3 (`h3` over this transport); an external client SDK
  that runs quinn on the host and speaks user schemas over relayed
  streams.
