# Design

## Context

Current-state facts that shape the approach (see proposal.md for motivation):

- The runtime drives spawned guests by calling the `__selium_guest_poll` export. A spawned guest's entrypoint future runs on the cooperative reactor and, on completion, is silently dropped: no `ProcessExited`, no teardown. Only bootstrap failure, trap, or parent `stop` emit lifecycle events — so a bridge-channel whose pipe tears down never exits (the "zombie reactor").
- Host quota counters exist (`QuotaSet`/`QuotaClear`, enforced at region / pipe / storage chokepoints) and are accountant-authored. There is no `Process` dimension: `ProcessStart` performs capability and grant checks but no quota check, and the accountant authors ceilings only for `SharedRegion`, `DurableLog`, and `BlobStore`.
- Because there is no child-exit signal, the bridge-server keeps a guest-local, lifetime `SpawnBudget` that never refills.
- The connector allocates a two-ring region plus a relay task for every accepted stream, with no per-connection stream cap and no admission rate limit.

## Goals / Non-Goals

**Goals:**

- Entrypoint completion becomes the process lifetime: completed guests exit and are fully torn down.
- A per-tenant `ResourceClass::Process` quota, default 100 and operator-raisable, enforced at spawn and released at exit.
- Remove the bridge's redundant lifetime budget; a quota-denied spawn surfaces to the client as a stream close.
- Edge admission limits (per-connection bidi-stream cap, per-tenant token bucket) that refuse before region allocation.

**Non-Goals:**

- Supervisor-driven reaping stays last-resort for stuck/trapped guests; this change does not build it.
- A quota-authoring UI (cloud tooling) — day-1 ceilings are accountant-authored defaults raised through existing operator policy.
- Cross-host / cluster-wide rate limiting; bandwidth rate limiting (already deferred by the accountant spec).

## Decisions

**D1 — Entrypoint completion is signalled through the poll export's return code.**

The entrypoint future is already the process's lifetime. The reactor marks the poll-owner task; when its join completes, the reactor reports "done"; `__selium_guest_poll` returns `i32` (`0` running, `1` done) instead of unit; the runtime's poll loop reads `I32(1)` and reaps. A parked entrypoint (a long-running service) reports running and stays resident.

*Alternatives considered:* a dedicated self-exit hostcall — rejected: a signal the reactor already has, and easy for a guest author to forget. Supervisor polling to detect idle guests — rejected: that is the last-resort path for broken guests, not normal teardown.

**D2 — Completion reuse the existing teardown path.**

`cleanup_failed_process` already records `ProcessExited`, revokes the process's discovery URIs, and reclaims regions and queued pipe slots. Completion reap funnels into that same path (extended in D3 to also release the process slot).

**D3 — Process quota is a first-class dimension keyed by the child's tenant.**

`ProcessStart` runs `enforce_quota(resolved_child_tenant, Process, 1)` after spawn-tenant resolution and before instantiation; a denial fails with `QuotaExceeded` naming the tenant and Process. Teardown releases the slot via `quota().release(tenant, Process, 1)`. Tenant-less root processes (system guests) are not metered, mirroring region/pipe quota principal resolution.

*Alternatives considered:* bridge-local counting — rejected, that is the current blunt lifetime cap and cannot see cross-identity aggregate signal. A platform-wide per-guest-kind cap — a distinct host-protection concern, out of scope here.

**D4 — The accountant authors the Process ceiling (default 100).**

`author_enforcement` adds `quota_set(tenant, ResourceClass::Process, ceiling)`; the ceiling defaults to 100 and is raised through the same operator policy that sets plan/overage ceilings; delinquency zeroes it with the other dimensions.

*Alternatives considered:* a hard-coded constant in the runtime — rejected, it moves policy into the host and removes the raise-on-request affordance.

**D5 — The bridge drops `SpawnBudget` and trusts the quota.**

On `Process::start_for_tenant` failure the bridge attach-then-closes the stream (the connector observes EOF), exactly like unknown-identity refusal. No guest-local spawn counter remains; the tenant's accountant-authored ceiling is the bound.

**D6 — Connector: per-connection cap plus per-tenant token bucket, applied before allocation.**

Set `max_concurrent_bidi_streams` on the server `TransportConfig` — quinn enforces the limit natively (a client cannot open streams beyond the advertised MAX_STREAMS). A token bucket keyed by the authenticated client's tenant (falling back to the resolved serving tenant when mTLS is off) refills on the host clock inside the accept loop; when empty, the stream is reset with a distinct error code (the connection stays up) before `QuicChannel::allocate`.

*Alternatives considered:* connection-count cap — rejected, too coarse given QUIC multiplexes many streams per connection. Bridge-side rate limiting — rejected, the region and handoff have already been paid by then.

## Risks / Trade-offs

- **[A guest that returns `Ok` now exits]** → Audit shows current system guests either loop forever or (bridge-channel) return on completion; a guest that wants resident background work must keep its poll-owner future alive. Documented in the `selium-guest` guidance.
- **[Quota denial must be distinguishable and informative]** → Reuse `AbiErrorCode::QuotaExceeded` with a message naming the tenant and the Process dimension; the bridge converts the failed spawn into attach-then-close, which the client observes as a clean stream close.
- **[Rate-limit key ambiguity when mTLS is off]** → Fall back to the resolved serving tenant, so public endpoints are bounded per route rather than per user. Documented.
- **[Per-key bucket growth]** → Bounded by active tenants; idle keys are evicted on refill.
- **[Legitimate process-heavy tenants surprised by the ceiling]** → That is the point of a default 100 with an operator raise path; metering already surfaces process counts.

## Migration Plan

1. Runtime + macros: completion signal and reap-on-completion (guests unchanged).
2. Runtime: `Process` quota at `ProcessStart` + release at teardown (no ceiling authored yet → effectively unlimited).
3. Accountant: author the default Process ceiling (100).
4. Bridge: remove `SpawnBudget`; treat `QuotaExceeded` spawn failure as a refusal.
5. Connector: per-connection stream cap + per-tenant token bucket.

Each layer is independently deployable and reversible; step 2 before step 4 keeps spawns unlimited until the ceiling exists.

## Open Questions

None — the deferrable specifics (exact stream-cap and rate/burst values) are operator configuration, not spec behaviour.
