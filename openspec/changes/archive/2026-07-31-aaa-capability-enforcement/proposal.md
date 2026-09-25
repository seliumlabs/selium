# Proposal: AAA Capability Enforcement

## Why

The capability model in `selium-abi` is well-formed (capabilities ×
selectors × intersection semantics), but the enforcement side is an
honest-to-goodness facade:

- **Selectors that can never match**: `Runtime::require` hardcodes
  `tenant: None, uri: None, locality: Cluster` when building the
  `ScopeContext`. `Tenant` and `UriPrefix` selectors therefore match
  nothing — a tenant-scoped grant passes spawn validation, then every
  hostcall fails with PermissionDenied at runtime (and, until the spine
  fix, blamed the wrong capability in the error).
- **Isolation by obscurity**: resource ids are sequential u64s (1, 2,
  3…). `AttachRegion` checks capability but not ownership, so any guest
  with a plain `SharedMemory` grant can attach to any other guest's
  regions by guessing ids — cross-process memory isolation currently
  rests on nothing.
- **Grant validation contradicts enforcement**: spawn-time
  `validate_grants` rejects empty selector lists, while enforcement
  treats empty selectors as "allow all" — so the only usable selectors
  today are class-level (`ResourceClass`, `Locality`), which nobody
  documents.
- **Unusable scopes**: `MeteringRead`/`GuestLogRead` require
  `ExplicitResource(Local(pid))` grants a supervisor can't plausibly
  hold in advance; `GuestLogWrite` with one's own pid fails ownership
  while `None` passes (inverted intuition).
- **Ownership bookkeeping bugs**: `cleanup_failed_process` deletes entire
  owner sets when one co-owner fails.

"Multi-tenant from day 1" is a stated design pillar; today there is no
tenant. This change makes the enforcement honest: either a selector is
evaluatable and enforced, or it is rejected loudly at grant time.

## What Changes

- **Real scope contexts**: every hostcall dispatch builds `ScopeContext`
  from the process's actual authority (tenant, URI of the target resource
  where known, locality, class, identity). Tenant identity enters the
  model via process authorities (grants and/or module-level tenant
  assignment), not ambient strings.
- **Fail loud at grant time**: grants containing selectors the runtime
  cannot evaluate in a context it will actually build (initially:
  `Tenant`, `UriPrefix` until wired) are rejected at spawn with a precise
  error — never accepted-then-always-denied. Empty-selector grants are
  accepted explicitly as "unrestricted within capability" and documented.
- **Unguessable identities**: resource ids become non-sequential
  (randomised or hashed with per-host mixing), and `AttachRegion` requires
  ownership (or an explicit `ExplicitResource` grant) — guessing no
  longer helps.
- **Ownership on attach/read**: shared-region attach and privileged reads
  (metering, guest logs, activity) check ownership or a matching
  `ExplicitResource` grant; add a `Descendants` selector or equivalent so
  supervisors can be granted over children sensibly.
- **Fix the bookkeeping**: `cleanup_failed_process` removes only the
  failed process from owner sets; `GuestLogWrite` ownership accepts the
  writer's own pid.
- **Honest capability matrix**: docs table of capability × selector ×
  enforced-or-rejected, generated or hand-maintained in the ABI crate.

### Explicitly out of scope

- Multi-tenant resource quotas/billing (metering exists; policy later).
- Network-level authentication (mTLS identity arrives with the bridge).
- Delegation/attenuation UX beyond child-grant containment.
- Audit logging format beyond the existing activity log.

## Capabilities

### New Capabilities

- `capability-enforcement`: evaluatable scope contexts, grant-time
  selector admission, unguessable identities, ownership-checked attach,
  and a documented enforcement matrix.

### Modified Capabilities

- `selium-abi`: grant/selector semantics SHALL distinguish evaluatable
  selectors from rejected ones; document intersection semantics.
- `selium-runtime`: enforcement SHALL build real scope contexts and check
  ownership on attach and privileged reads; cleanup SHALL preserve
  co-owners.

## Impact

- `crates/core/abi`: docs + possibly new selector variant(s).
- `crates/core/runtime`: `require`, id allocation, attach/read checks,
  cleanup, grant admission at spawn/ProcessStart.
- `crates/core/kernel`: id generation support.
- `crates/guests`: supervisor/discovery grant expectations updated.
- README/AGENTS: the capability section reflects the enforcement matrix.
