# Proposal: DNS Connector (Typed Egress Resolution)

## Why

`guest-net-sockets` makes `TcpConnect` literals-only, which removes the
host's ambient DNS authority — but guests still need names resolved.
Today that gap would pressure us to re-admit host-side resolution (and
its ambient-authority problems: unconstrained lookup triggering, grants
constraining names the guest cannot verify). The connector model offers
the honest version: name resolution is itself a typed, capability-gated
RPC to an egress DNS connector — a system guest holding the UDP network
grant — instead of an ambient host behaviour.

## What Changes

- **`selium-proto-dns`** (new crate): schema types (`DnsQuery`,
  `DnsResponse`, record types) via `selium-encoding`.
- **`selium-connector-dns`** (new system guest): plain guest on the raw
  `UdpSocket` SDK. Receives typed `DnsQuery` over a channel, performs
  real DNS over UDP/53 to a configured resolver, returns typed
  `DnsResponse`.
- **Reply correlation without a framework**: in-flight map keyed by
  `(transaction id, client address)` ↔ frame tag; datagrams carry binary
  source addresses per the `udp-transport` format.
- **Guest-facing resolve API**: a thin client in the SDK
  (`net::resolve(name) -> Result<Vec<IpAddr>>`) that is just an RPC
  client to the connector's well-known discovery URI. No guest code
  touches DNS wire format.
- **Capability story**: resolving guests hold a channel grant for the
  connector's well-known URI, not `Network`. The connector holds
  `Network + UdpSocket` (+ `UriPrefix("udp://<resolver>:53")` once
  `network-capability-uris` lands).

## Capabilities

### New Capabilities

- `dns-connector`: typed egress DNS resolution as a connector —
  query/response forwarding, transaction correlation, and the capability
  model replacing ambient host DNS

### Modified Capabilities

(None.)

## Impact

- New crates: `selium-proto-dns`, `selium-connector-dns` (guest).
- `selium-guest`: `net::resolve` client helper.
- Depends on: `guest-net-sockets` (UDP socket SDK + binary datagrams).
- Synergy: completes the literals-only story — guests resolve via the
  connector, then connect by literal.
- Out of scope (documented): response caching, DNS-over-TCP fallback for
  truncated responses, DNSSEC validation, search-domain expansion.
- Until this lands, testing uses `127.0.0.1` literals — no host
  resolution is reintroduced in the interim.
