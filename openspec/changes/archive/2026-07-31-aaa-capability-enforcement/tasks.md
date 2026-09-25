# Tasks: AAA Capability Enforcement

## 1. Scope contexts and tenants

- [x] 1.1 Add `tenant: Option<String>` to `ProcessAuthority`; thread through spawn (inherit or assign) and `ProcessStart` containment.
- [x] 1.2 `Runtime::require` builds `ScopeContext` from the authority (tenant, resource URI where known, locality, class, identity); remove the hardcoded `None`s.
- [x] 1.3 Tenant-scoped grant end-to-end test: two tenants, same operation, one allowed one denied, with correct error attribution.

## 2. Grant admission

- [x] 2.1 Replace `validate_grants` with the admission matrix (admitted: ResourceClass, Locality, ExplicitResource, Tenant; rejected: UriPrefix until contexts populate; empty = unrestricted-in-capability).
- [x] 2.2 Precise spawn errors naming the rejected selector; regression test for the accept-then-deny trap.
- [x] 2.3 Publish the enforcement matrix in `selium-abi` docs and README.

## 3. Identities and ownership

- [x] 3.1 Randomised shared/local id allocation (retry on collision; test-seeded determinism); remove sequential counters.
- [x] 3.2 `AttachRegion` requires ownership or `ExplicitResource(Shared(id))`; queue-send ownership sharing at recv (documented, kernel-side).
- [x] 3.3 Isolation test: guest without a grant cannot attach to another guest's region by guessing ids; with `ExplicitResource` it can.

## 4. Privileged reads and bookkeeping

- [x] 4.1 New `Children` selector (or equivalent) for metering/activity/guest-log reads of descendant processes; `GuestLogWrite` accepts the writer's own pid.
- [x] 4.2 Fix `cleanup_failed_process` to preserve co-owners; regression test.

## 5. Gates

- [x] 5.1 Adversarial test pass: attach-guessing, selector traps, child containment, cleanup races.
- [x] 5.2 Gates: fmt, clippy `-D warnings`, full suite, wasm32 builds, spine test green; docs updated.
