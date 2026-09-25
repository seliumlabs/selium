# Proposal: Connector Channel Forwarding

## Why

Edge connectors forward typed requests to app guests over shared-memory
channels, but the mediation path between them is undesigned composition:
discovery resolution, host-queue attach, per-session region rendezvous,
and attach-grant derivation each exist as separate deployed capabilities
with no owner of the composed flow. Two gaps are load-bearing today: no
requirement governs `HostQueueAttach` on another process's queue (the
exact operation every connector performs on every route), and no
requirement defines region lifecycle for the connector's
allocate-per-connection pattern. Without these, connector routing works
by accident and the http-connector change's zero-Network-grant security
story rests on unstated behaviour.

## What Changes

- **Specify the composed forwarding flow**: discovery lookup of a served
  URI subtree → `ResourceSender::attach` on the resolved queue →
  per-connection multi-memory region allocation by the connector →
  rendezvous delivery → `rpc::accept` server side. Composes existing
  primitives (`discovery-registration`, `secure-rpc`,
  `capability-enforcement`); introduces no new transport mechanism.
- **Authorize `HostQueueAttach`**: attach to a host queue owned by
  another process SHALL require an `ExplicitResource` grant naming that
  queue or receipt of the queue descriptor through discovery under a
  granted URI. Ungranted attach attempts are denied loudly.
- **Define connector region lifecycle**: the connector allocates one
  request/reply region pair per forwarded external connection, reclaims
  it when the external peer disconnects or the serving guest closes, and
  never shares a region across external connections.
- **Define stale-route failure semantics**: a route resolved from a stale
  discovery cache entry fails loudly at queue attach (grant miss /
  missing queue) — requests are never misrouted to an unintended guest.

## Capabilities

### New Capabilities

- `connector-channel-forwarding`: the runtime-mediated path from an edge
  connector's route resolution to a live typed RPC session with the
  serving app guest — queue attach authorisation, per-connection region
  lifecycle, and failure semantics for the composed flow

### Modified Capabilities

- `resource-handshake`: adds a requirement that `HostQueueAttach`
  against a queue owned by another process is capability-checked;
  previously only `HostQueueSend` carried validation requirements

## Impact

- **Specs**: new `connector-channel-forwarding`; delta on
  `resource-handshake`.
- **Runtime/kernel**: `HostQueueAttach` gains a capability check in the
  hostcall handler (currently unvalidated); one restricted hostcall is
  added (`RecordResolvedQueueFor`) so the discovery service can report
  resolve results on behalf of resolvers — callable only by the
  discovery guest.
- **Guest SDK**: no API changes — `ResourceSender::attach` and
  `selium_shm::rpc::connect/accept` already express the flow; behaviour
  tightens where attaches were previously unconditionally permitted.
- **Depends on**: `discovery-registration` (route resolution),
  `secure-rpc` (session regions, correlation),
  `capability-enforcement` (ownership-checked attach — its queue-handoff
  derivation rule is the basis for receiver rights on forwarded regions).
- **Consumers**: `http-connector` (unblocks tasks 4.1–4.4 golden-path
  CI); future DNS/WebSocket/SMTP connectors reuse this unchanged.
