# Design: Connector Channel Forwarding

## Context

The forwarding substrate composes four deployed capabilities:
`discovery-registration` (route resolution), `resource-handshake`
(host-queue rendezvous), `secure-rpc` (per-session regions, typed
correlation), and `capability-enforcement` (ownership-checked attach,
including its queue-handoff derivation rule). The http-connector
implementation already exercises this composition in placeholder form;
this change makes the composition contractual and closes the two
enforcement holes found while speccing it. See proposal.md for
motivation.

Current-state facts that constrain the design:

- `HostQueueAttach` is currently unvalidated in the runtime hostcall
  handler — any process can attach to any queue by id.
- `HostQueueSend` IS validated (`resource-handshake`), and
  `AttachRegion` is validated via ownership/grant/queue-handoff
  (`capability-enforcement`).
- Discovery resolution currently performs no authorisation filtering
  (tenant scoping is a known TODO in the discovery guest); resolution
  returns whatever is registered.
- Per-connection regions are allocated client-side by
  `selium_shm::rpc::connect` today; no new mechanism is needed.

## Goals / Non-Goals

**Goals:**

- Make queue attach authorisation explicit and enforced
- Make connector region lifecycle contractual (allocate/reclaim)
- Keep the zero-Network-grant app-guest story end-to-end auditable

**Non-Goals:**

- Authorisation filtering inside discovery lookups (separate concern;
  tenant scoping remains a discovery-guest TODO)
- Transport-level ABI changes (the one added hostcall is a
  runtime-mediated authorisation record, not a new transport mechanism)
- Connector concurrency model (per-listener scoping stays as designed in
  http-connector)

## Decisions

### Discovery resolution as a delegation basis for queue attach

Authorisation basis for cross-process `HostQueueAttach`: (a)
`ExplicitResource` grant naming the queue, or (b) the caller obtained
the shared id through its own successful discovery Resolve.

Why: discovery already mediates *intended* publication — registering a
URI under a subtree is an act of delegation by the owner. Treating a
successful resolve as an authorisation basis keeps connectors grantless
with respect to individual app guests (they cannot pre-declare grants
for guests that register later) while still denying blind id-guessing.
Option (b) alone would make revocation semantics fuzzy; option (a)
alone breaks dynamic registration. Both together give static
deployments a non-discovery path.

Alternative considered: require discovery to return a scoped,
single-use attach token instead of the raw shared id. Rejected for now
— it changes the ABI payload shape and duplicates what unguessable ids
(`capability-enforcement` "Unguessable Resource Identities") already
provide; revisit if revocation-abuse patterns emerge.

Implementation surface: the runtime records, per process, the set of
queue ids returned to that process by successful Resolves (a small
HashSet on the process authority record). Because resolution executes
inside the discovery guest rather than a runtime hostcall, the
discovery service reports each resolve result via a
`RecordResolvedQueueFor { client_process_id, shared_id }` hostcall that
the runtime accepts **only** from the booted discovery process — a guest
can never assert its own resolves. `HostQueueAttach` consults: owner? →
allow; granted? → allow; resolved-by-caller (as recorded by discovery)?
→ allow; else deny.

### Receiver rights ride the existing handoff rule — no new grant mutation

When the serving guest receives a region id via `HostQueueRecv`,
`capability-enforcement`'s queue-handoff scenario already specifies
ownership sharing at receive time. We do not inject `ExplicitResource`
grants into the guest's authority table at rendezvous delivery.

Why: zero runtime bookkeeping per forwarded connection (connectors may
forward thousands/sec), and the deny property we need ("third party
cannot claim handoff rights") follows from queue attachment being
authorized, which the previous decision secures.

Alternative considered: runtime-injected per-region grants. Rejected —
mutating authority tables at message-delivery rate adds contention and
a failure mode (grant leak on crash between send and revoke) with no
security gain over receive-time sharing.

### Region lifecycle owned by the connector as allocator

`rpc::connect` already allocates the region pair client-side. The spec's
lifecycle requirement lands on the connector because it is the party
that observes both endpoints: external peer disconnect (socket EOF) and
serving-guest close (`ConnectionClosed` on the reply ring). The client
handle returned by `rpc::connect` owns its session region and frees it
via `FreeRegion` on drop, so reclaimation happens on whichever event ends
the forwarding loop first; the free is ownership-checked host-side.
Freeing reuses the existing `FreeRegion` path, which also publishes
discovery revocations — stale route-cache entries then fail naturally
at next attach, satisfying the stale-route requirement without a cache
TTL.

No reference counting across the two rings: the region pair is one
allocation (`create_rpc_region` returns one parent region). One free
call reclaims both.

### Stale-route handling: fail-per-request, invalidate-on-miss

On queue-attach failure, the connector removes the cache entry and
returns a typed error response (connector-generated, e.g. 502-equivalent
for HTTP) for that request only. No global cache flush, no retry loop.

Why: cheap, bounded, and correct — a revoked route fails once loudly,
the next request re-resolves fresh. A TTL was considered and rejected:
it trades latency of invalidation against timer machinery, and the
attach failure is already an exact invalidation signal.

## Risks / Trade-offs

- [Resolve-as-basis widens attach if discovery registration is abused]
  → Registration is ownership-checked (tier-2 validation); a guest can
  only publish queues it owns. An attacker must control a registered
  queue to gain attach, which means they already own it.
- [Runtime-side resolve-record set grows with distinct routes]
  → Bounded by distinct URIs resolved per process, not per request;
  entries die with the process. If profiling shows pressure, cap with
  LRU eviction (losing only the re-attach shortcut, not security).
- [Connector crash leaks session regions until guest close]
  → Serving guest observes writer-count-zero `ConnectionClosed` and
  drops its handle; kernel reclaim on last detach covers the residue.
  Same semantics as any RPC client crash today.

## Migration Plan

Additive enforcement tightening:

1. Add resolve-record tracking + `HostQueueAttach` check in runtime.
2. Fix any existing test fixtures that attach to foreign queues without
   basis (expected: few — most use their own queues or the discovery
   handle wired at bootstrap, which is resolve/ownership-based).
3. Land connector wiring (http-connector tasks 4.x) after this change.

Rollback: revert the hostcall check; no data or format migration
involved.

## Open Questions

None blocking. Tenant-scoped discovery filtering remains open in the
discovery guest but does not affect this change's contracts.
