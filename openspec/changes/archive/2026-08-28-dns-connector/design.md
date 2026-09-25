# Design: DNS Connector

## Context

`TcpConnect` is literals-only (`guest-net-sockets`), so resolution must
exist somewhere honest. DNS maps cleanly onto unary RPC — the simplest
member of the connector family and the proving ground for its
conventions before `http-connector`. See proposal.md for motivation.

## Goals / Non-Goals

**Goals:**

- Name resolution as a typed, capability-gated RPC
- No ambient DNS anywhere in host or runtime code paths
- Correlation discipline reusable as the connector reference example

**Non-Goals:**

- Caching, TTL handling, negative-caching
- DNS-over-TCP fallback, DNSSEC, search domains
- mDNS/LLMNR or any listener-facing DNS (this is egress only)

## Decisions

### Egress connector, not a runtime service

Resolution lives in a guest so policy (which resolver, which tenants may
resolve) is guest code per non-negotiable 5. The runtime never resolves;
the literals-only enforcement in `guest-net-sockets` stays absolute.

### Correlation: `(txid, client addr) → frame tag` in-flight map

The connector allocates the DNS transaction id per upstream query and
remembers `(txid, resolver addr) → (reply channel, tag)`. Replies are
demuxed by exact map lookup; unknown txids are dropped. This is the
Q4 pattern in its smallest form — a HashMap, not a framework.

### Well-known discovery URI

The connector registers a well-known `sel://` URI (e.g.
`sel://sys/dns/resolve`); guests attach via discovery. Granting
resolution = granting that channel — tenant-scoped policy falls out of
the existing selector algebra.

### Failure honesty

Upstream timeout, NXDOMAIN, and truncation each map to distinct typed
`DnsResponse` outcomes; the connector never silently retries forever
and never fabricates answers.

## Risks / Trade-offs

- Connector unavailability = resolution unavailability for all tenants →
  supervisor restart policy; the resolve API surfaces unavailability as
  a typed error, never a hang.
- Amplification/abuse via the connector → grant-gated channel plus
  per-tenant metering hooks already in the ABI; rate policy deferred to
  a later hardening change.
