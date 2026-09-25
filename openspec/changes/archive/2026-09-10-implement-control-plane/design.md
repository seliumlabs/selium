# Design: Implement the Control-Plane Guest

## Context

See `proposal.md` for motivation. The relevant substrate facts that shape the
approach:

- The bridge (`connector-quic` → `bridge-server` → `bridge-channel`) splices an
  external client's stream into a **shared-memory channel** resolved by URI.
  That splice is a shared-frame relay: it fits pub/sub and live-table
  targets, where every attached reader legitimately shares one ring. RPC is
  point-to-point and session-per-client: the internal path allocates a
  two-ring session region client-side (`rpc::connect`) and enqueues its
  shared id into the server's host queue (`rpc::accept`) — hostcalls an
  external QUIC client cannot make. No guest yet serves an RPC surface to
  the outside; the bridge must rendezvous on the external client's behalf.
- Internal guest-to-guest RPC runs over **host queues** via `Context` (the
  pre-connected discovery `OwnedRpcClient`) and `selium_shm::rpc`, which is
  `selium_wire::rpc` over `ShmTransport`. All RPC payloads are `FlatMsg`
  (FlatBuffers); rkyv is confined to the ABI/feed layer.
- The storage capability hostcalls (`StorageBlobPut`/`Get`,
  `StorageLogAppend`/`Replay`, blob manifests) already exist in `selium-abi`.
- `selium-scheduler`, `selium-supervisor`, and `selium-cluster` are specified
  in `implement-system-guests` but not yet implemented; external-api has a
  partial implementation with a text protocol over a raw `TcpListener`.

## Goals / Non-Goals

**Goals:**
- One typed, capability-gated control surface for externally authenticated
  clients, reached through the bridge — which this change extends with an
  RPC rendezvous for host-queue targets.
- A control-plane guest that owns user-facing desired state and delegates all
  platform policy.
- Reuse the existing feed/table/RPC overlays rather than introducing a new IPC
  noun or a storage service.

**Non-Goals:**
- The `sel` CLI crate (a follow-up consuming this surface).
- Cluster-wide telemetry aggregation and data-visualisation read models.
- A label/scope selector for the capability system.
- Placement, recovery, or discovery-naming policy inside the control plane.

## Decisions

### 1. Repurpose `selium-external-api` as `selium-control-plane`

**Decision:** Rename the crate and guest, keep the crate directory shape
(`guests/control-plane`, package `selium-control-plane`), and supersede the
text-protocol implementation. The surviving semantics from the external-api
spec — decomposition, delegation, error propagation — are re-expressed over
typed messages in the `control-plane` capability.

**Rationale:** The spec-level intentions (narrow interpretation, delegation)
were right; the text wire format and raw-TCP listener are the parts DESIGN-INTENT
rejects. A rename plus re-spec, rather than a second guest, avoids two competing
external surfaces.

**Alternative considered:** Keep external-api and add a separate control-plane.
- Rejected: two user-facing surfaces for the same role; fragments authority.

### 2. The control plane is "thin" and "stateful" on two different axes

**Decision:** The control plane owns **user-facing desired state** (what users
asked for) but does **not** decide platform policy. It stores deployments and
pipeline bindings, and it translates intent into delegated interactions.

**Rationale:** "Inspector" verbs need a stable, single place to read user
intent; "mutator" verbs need one place to write it. Platform decisions stay
with scheduler (placement), supervisor (recovery), and discovery (naming).

**Alternative considered:** A stateless facade that fans out per request.
- Rejected: no owned read projection; inspector verbs would fan out and
  synthesize on every query.

### 3. Serve a host-queue listener; the bridge performs the RPC rendezvous

**Decision:** The control plane serves the same host-queue listener pattern
as discovery: it creates its own host-mediated connection queue
(`ResourceListener::create`), registers it via `Context::serve` under
`["control"]` — internal path `sel://<tenant>/control`, external wire name
`control.<tenant>` — and accepts typed RPC sessions with
`selium_shm::rpc::accept`. External clients reach it because the bridge
becomes protocol-aware: `bridge_pipe` dispatches on the discovered target's
class. Channel targets keep the existing transparent splice (pub/sub and
live-table targets unchanged). Host-queue targets take a bridge-side
rendezvous: the bridge-channel allocates a two-ring session region
(mirroring `rpc::connect`), splices the client's stream into it, and
enqueues the session's shared id into the served queue via
`ResourceSender` (the handoff metadata is empty until guest-spawned
children can receive pointer arguments — today's `Process::start` carries
integers only — with authorization resting on the runtime's attach
enforcement); on teardown the bridge frees the session region so the
server observes session end, mirroring `OwnedRpcClient`'s drop semantics.

**Rationale:** RPC is session-per-client. Serving a single registered
channel cannot express that: every external client splices into the same
ring, ring readers race, so a reply frame is delivered to an arbitrary
client — whose `RpcClient` discards the foreign correlation tag — and the
reply is lost. There is also no arrival signal by which the guest could
mint a per-client channel. The rendezvous, by contrast, is exactly the
mechanism the platform already uses for guest-to-guest RPC; the
bridge-channel is a hostcall-capable guest, so it can play the client role
for external streams. Discovery lookup already marks the queue attachable
for the bridge (`RecordResolvedQueueFor`), so no new grant or enforcement
semantics are required.

**Alternative considered:** Serve a channel and run a frame loop over it.
- Rejected: the shared-ring reply-routing problem above turns the control
  surface into a lossy bus. (An earlier draft of this design rejected the
  rendezvous on the grounds that "serving a channel works with the bridge as
  built" — that holds only for shared-frame broadcast patterns, not for RPC
  sessions; the rejection is reversed here.)

### 4. Durable log is truth; live tables are read models; blobs hold modules

**Decision:** Accepted intents are appended to the control plane's durable log
(the replayable record); deployment and pipeline projections are live tables
materialised from it. Uploaded module bytes go to the blob store with a
manifest name; deployments reference the manifest.

**Rationale:** Matches the ABI's log→replay→projection shape and keeps the
recovery story restart-based (replay the log, rebuild the projections). Blobs
are the right canonical home for opaque, checksummed module bytes; a live table
is a records model, not a byte store.

**Alternative considered:** Live tables as the store of record.
- Rejected: inverted replay direction, version bloat, and no compaction story.

### 5. Narrow delegation set

**Decision:** The control plane's delegated interactions are concrete and
bounded:

- **discovery** — `Context::lookup` / `Context::serve` / `Context::register`
  over the pre-connected host-queue RPC.
- **scheduler** — a typed `SchedulerRequest`/`SchedulerResponse` RPC client
  (now live in `selium-abi`, not the external-api stub), once the scheduler's
  RPC service exists.
- **supervisor** — consumed as a health/recovery **projection**, not commanded:
  the supervisor is by design a reactive control loop, so the control plane
  reads its state for status and never sends it imperative recovery commands;
  user verbs that look like recovery decompose into desired-state writes
  through the scheduler.

**Rationale:** Preserves the supervisor's autonomy and keeps the "dumb
client, policy in domain guests" boundary honest.

**Alternative considered:** Command the supervisor directly for recovery.
- Rejected: inverts the supervisor's designed control-loop direction.

### 6. Authorize via capability enforcement, not re-derived identity

**Decision:** The control plane authorizes by consulting the runtime for the
caller's tenant and capabilities (`process_tenant` / `process_capability`) on
the bridge-channel peer it sees, plus the capability checks the runtime already
enforces at attach time. It does not decode TLS identity itself.

**Rationale:** The bridge channel holds the client's conferred grants, so the
runtime already has an honest enforcement basis; the guest only queries it.
Identity → grants remains the identity guest's job (deferred), not the control
plane's.

**Alternative considered:** Have the bridge pass the fingerprint to the guest.
- Rejected: couples the guest to a transport identity format and re-implements
  authorization where the capability system already owns it.

### 7. Presentation hierarchy is a label projection; containment stays with the runtime

**Decision:** The control plane projects any future directory/hierarchy view
from `ResourceTarget.labels`, never as URI path segments. It never mirrors
resource containment; that remains the runtime's authority (a future observe
feed), which the control plane consumes and projects.

**Rationale:** Keeps flat `sel://<tenant>/<type>/<id>` addressing and the
"labels for classification, not hierarchy" decision intact, and keeps exactly
two sources of truth (discovery = index, runtime = containment).

**Alternative considered:** Reintroduce path-like grouping under the control
plane.
- Rejected: reopens the rejected arbitrary-path-hierarchy decision.

### 8. Day-1 surface is desired-state verbs

**Decision:** The first served surface covers resolve, upload (module),
deploy/scale/stop (desired-state writes), and status reads. Bare direct-ops
(`start`/`stop` of a single replica outside named desired state) is deferred.

**Rationale:** Desired-state verbs exercise the full own-then-delegate path and
the durable projection; direct-ops is a later, simpler addition once
reconciliation works.

## Risks / Trade-offs

- **[First bridge-rendezvoused RPC target]** The bridge-channel's host-queue
  rendezvous is new bridge semantics; the control plane is its first served
  target. → Mitigation: target-class dispatch keeps the SharedRegion splice
  path byte-identical (existing pub/sub and live-table bridge tests are the
  regression suite); a native test seam mirrors `bridge_pipe` (stream →
  session region → queue enqueue → `rpc::accept` round trip) before WASM
  integration.
- **[Scheduler RPC dependency]** Delegation to the scheduler is blocked until
  the scheduler's RPC service lands. → Mitigation: typed stub in `selium-abi`,
  replaced by the real client in the same sequence the old external-api
  planned; `deploy` returns a typed "delegated-but-not-applied" status until
  then.
- **[Control-plane spawn lifecycle]** Per-tenant control-plane instances, like
  bridge servers, have no spawn/restart owner yet. → Mitigation: document as a
  deliberate day-1 boundary; runtime `SystemGuestDescriptor` bootstraps it,
  supervisor ownership is follow-up.
- **[Desired-state durability is in-memory]** The durable log hostcall is
  implemented in-memory today. → Mitigation: the design does not depend on
  disk persistence; the log surface is the same, so disk backing lands later
  without re-spec.
- **[Grant matrix]** Exact grants (Storage, SharedMemory, HostQueue) must be
  scoped so a client's data-plane grants cannot reach the control surface. →
  Mitigation: control-plane attach is gated on a control-plane-covering
  capability/resource class; grants are reviewed in tasks (8.4-style
  authority-boundary validation).

## Migration Plan

1. Rename `guests/external-api` → `guests/control-plane` (`selium-control-plane`);
   remove the text-protocol parser and `TcpListener` wiring.
2. Add the control-plane message schemas (`ControlRequest`/`ControlResponse`)
   as FlatBuffers bindings in `selium-abi` (mirroring `DiscoveryRequest`).
3. Implement the entrypoint: create the host-queue listener, `serve` the
   control route, attach the `rpc::accept` loop, mark ready after
   registration.
4. Implement desired-state ownership (durable log append + live-table
   projection) and the delegation dispatcher (discovery now, scheduler stub).
5. Wire module upload through `StorageBlobPut` + manifest.
6. Implement the protocol-aware bridge: target-class dispatch in
   `bridge_pipe`; host-queue targets get session allocation, splice, queue
   enqueue, and teardown parity.
7. Re-enable workspace membership and add native tests + (once the other system
   guests exist) a bootstrap integration test.
8. Supersede `implement-system-guests` §7 (external-api).

**Rollback:** Revert the crate rename and re-point `implement-system-guests`
§7; no ABI additions are required before step 2, so the change is reversible
until the message schemas land. The bridge rendezvous (step 6) is contained
in `bridge_pipe`'s class dispatch; reverting it restores splice-only
behaviour without touching the control-plane guest.

## Open Questions

- **Scope/limits selector**: deferred; the design uses existing
  `Tenant`/`ResourceClass`/`ExplicitResource` selectors only.
- **Wire name vs internal path**: `control.<tenant>` / `sel://<tenant>/control`
  is proposed; confirm the label/projection convention once the data-vis UI
  work starts.
- **Direct-ops verbs**: deferred to a follow-up after reconciliation works.
- **Multi-instance sharding**: one control-plane per tenant for day 1; fan-in
  scaling is a cluster-scaling concern.
