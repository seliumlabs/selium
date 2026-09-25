# Design: AAA Capability Enforcement

## Context

`CapabilityGrant.allows(context)` implements intersection semantics over
selectors; the runtime builds the `ScopeContext` per hostcall. The model
is sound; the context construction and identity assignment are where
honesty is missing.

## Goals / Non-Goals

**Goals:**

- Every selector either enforces or is rejected at grant time.
- Cross-process memory isolation holds by default.
- The enforcement matrix is small enough to fit on one page.

**Non-Goals:**

- Quotas, billing, mTLS identity, delegation UX, audit format.

## Decisions

1. **Tenant as authority state, not strings on hostcalls.** A process
   authority gains `tenant: Option<String>` assigned at spawn (from the
   parent's tenant or explicit host assignment for system guests). Child
   grants must stay within the parent tenant (containment already exists;
   it now has something to contain).
2. **Admission at grant time.** `validate_grants` becomes a capability ×
   selector admission check against a static matrix of what the runtime
   evaluates. Initially admitted: `ResourceClass`, `Locality`,
   `ExplicitResource`, `Tenant`. `UriPrefix` is rejected until resource
   URIs populate contexts (arrives with discovery-driven attach).
   Empty selector list = explicitly unrestricted within the capability.
3. **Ids are unguessable and ownership is checked.** Shared ids switch
   from `fetch_add` counters to random u64 (retry on collision;
   determinism only where tests seed it). `AttachRegion` requires:
   caller owns the region, OR holds `ExplicitResource(Shared(id))`, OR
   the region was explicitly handed over (rendezvous patterns pass
   ownership on queue send — evaluated with the discovery-bootstrap
   slice's needs in mind; simplest rule first: owner or explicit grant).
4. **Privileged reads get a `Children` selector.** Metering/activity/
   guest-log reads accept `ExplicitResource(Local(pid))` or a new
   selector matching descendant processes of the grantee (supervisors
   spawn children and read their telemetry). `GuestLogWrite` treats the
   writer's own pid as owned.
5. **Cleanup preserves co-owners** (one-line bug fix with a regression
   test: two processes own a region; one fails; the other's ownership
   survives).

## Risks / Trade-offs

- **Rendezvous ownership transfers**: host-queue handoffs (RPC connect)
  pass region ids between processes; strict owner-only attach breaks them
  unless the send transfers or shares ownership. Rule chosen: queue send
  of a region id *shares* ownership with the receiving process at recv
  time (kernel-side, since the queue is host-mediated and trusted). This
  is the one place ownership is granted implicitly, and it is documented.
- **Random ids break debuggability** slightly (no more "region 7");
  mitigated by the discovery URI aliases already carrying pid+id.
- **Tenant-free single-tenant clusters** (the common case today) must not
  pay for the model: tenant `None` means "platform tenant", and
  Tenant-scoped grants simply don't match it — same rule as any other
  tenant, no special casing beyond docs.
