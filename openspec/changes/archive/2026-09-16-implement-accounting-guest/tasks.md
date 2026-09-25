## 1. ABI Surface

- [x] 1.1 Add `Capability::QuotaWrite` variant to `selium-abi` with rkyv derives, and verify `cargo test -p selium-abi` round-trip and admission-matrix tests pass
- [x] 1.2 Add `QuotaSet { tenant, class, limit }` and `QuotaClear { tenant, class }` variants to `HostcallRequest`, and verify encode/decode round-trip tests pass
- [x] 1.3 Add accountant service message types (bucket and control messages) to `selium-service`, and verify schema generation and codec tests pass
- [x] 1.4 Add the `AbiErrorCode::QuotaExceeded` variant (distinct from `PermissionDenied`) with a round-trip test, and map the kernel quota error to it

## 2. Capability Enforcement

- [x] 2.1 Implement `QuotaWrite` bootstrap-only provisioning and spawn-time non-conferability in the runtime admission matrix, and verify the capability-enforcement scenario (bootstrap grant + spawn denial) passes

## 3. Runtime Quota and Metering Producer

- [x] 3.1 Add a host-held tenant-scoped quota table to the kernel, and verify unit tests cover set/clear/lookup (usage tracked even before a ceiling is authored, inherited by `set`)
- [x] 3.2 Enforce quota counters synchronously at allocation dispatch sites — shared-memory bytes (`AllocRegion`), storage bytes (`StorageLogAppend`/`StorageBlobPut`), queued pipe items (`HostQueueSend` + kernel-side listener deliveries) — behind `QuotaWrite`, and verify over-ceiling allocations fail with `QuotaExceeded` naming tenant and dimension
- [x] 3.3 Add a per-second metering projector to the runtime (ticker started at bootstrap; TCP send/recv bandwidth instrumentation; CPU accounting is a host hook until the WASM-resources change), and verify observations update on the tick
- [x] 3.4 Release quota reservations at resource free and process teardown (region bytes, queued pipe items; storage is sticky — durable bytes persist, user-side management mechanism TBA), and verify the teardown test
- [x] 3.5 Transfer handed-off resources: queue handoffs move ownership (sender loses free rights) and the region's quota reservation follows the resource across tenants (force-accepted; poisoned-handoff vector documented in the spec), and verify the transfer test

## 4. Accountant Crate — Bookkeeper

- [x] 4.1 Create the `crates/guests/accountant` crate with `bookkeeper` and `accountant` entrypoints and interface metadata, and verify both descriptors boot from the same module bytes
- [x] 4.2 Implement the bookkeeper process inventory from lifecycle activity events and `ProcessTenant`, and verify it tracks live local processes
- [x] 4.3 Implement per-second `MeteringRead` sampling with counter differences and gauge sampling, and verify per-tenant bucket reduction against a synthetic process set
- [x] 4.4 Publish per-tenant buckets (stamped with their publish time) to a shared-memory topic, and verify the accountant entrypoint receives published buckets

## 5. Accountant Crate — Accountant

- [x] 5.1 Merge bookkeeper buckets into per-minute windows bucketed by the bucket's publish-time stamp, and verify windowed per-tenant usage is correct
- [x] 5.2 Append per-minute windows to a durable usage ledger with replay, and verify ledger state and enforcement re-authoring survive an accountant restart (`accountant_recovery` test)
- [x] 5.3 Evaluate plan/overage ceilings and write the per-tenant account state machine (paid, in-overage, at-budget, delinquent), and verify overage recording and delinquency transitions
- [x] 5.4 Author enforcement state from account state — `QuotaSet`/`QuotaClear` and narrowing publication (bandwidth rate bands are operator-authored policy deferred with the cloud management tooling; the accountant does not author them) — and verify quota values and narrowing follow state transitions

## 6. Bridge Narrowing

- [x] 6.1 Fold the accountant's published narrowing into the bridge-server's conferral (fail-open when the table is unreachable, per the amended spec), and verify bridge-channels receive baseline grants minus narrowing

## 7. Bootstrap and Integration

- [x] 7.1 Wire the bookkeeper and accountant `SystemGuestDescriptor`s (grants including bookkeeper `MeteringRead` and accountant `QuotaWrite`/`Storage`, readiness, dependency order) and verify the bootstrap-order test passes
- [x] 7.2 Validate the single-host loop end to end — a workload runs, buckets flow, the ledger records usage, and an over-ceiling allocation is denied — and verify the integration test passes (`accountant_spine`, with the guest log surfacing on failure)
