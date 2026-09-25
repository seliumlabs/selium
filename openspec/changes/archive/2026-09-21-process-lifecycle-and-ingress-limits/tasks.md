# Tasks

## 1. Entrypoint completion signal (selium-guest + macros)

- [x] 1.1 Track the poll-owner entrypoint task and detect its completion in the reactor; verify with a native test that a spawned future which completes causes the reactor to report completion while a parked future reports running
- [x] 1.2 Change the `__selium_guest_poll` export to return an `i32` completion code (0 running / 1 done) instead of unit; verify the macro expansion compiles for wasm and the generated export's signature is exercised by a macros integration test
- [x] 1.3 Make `run_entrypoint_with_result` mark late completion (Ok or Err) as a completion signal rather than leaving the task resident; verify with a late-completion test and the existing entrypoint-result macro tests

## 2. Runtime reaps completed guests

- [x] 2.1 Read the poll export's completion code in `poll_guest_once` and, on completion, record `ProcessExited` and tear the process down instead of re-inserting it; verify with a runtime test that a spawned guest whose entrypoint returns is removed from the process table with a `ProcessExited` event
- [x] 2.2 Route the completion reap through the existing teardown path (revocation of discovery URIs, region/pipe reclaim) without a divergent cleanup; verify the existing process-teardown revocation scenarios still pass and apply to the completion reap

## 3. Process spawn quota (selium-runtime)

- [x] 3.1 Enforce the tenant-scoped `ResourceClass::Process` quota in the `ProcessStart` hostcall after spawn-tenant resolution, denying over-ceiling spawns with `QuotaExceeded` before instantiation; verify with a unit test that a ceiled tenant's spawn is denied while a root (tenant-less) spawn is not metered
- [x] 3.2 Release the process-quota slot when a process exits or is reaped, in the existing teardown path; verify with a unit test that after a child exits its tenant can spawn again up to the ceiling
- [x] 3.3 Confirm the kernel quota counter accepts `ResourceClass::Process` with no new ABI variants; verify via the existing quota set/round-trip tests

## 4. Accountant authors the process ceiling

- [x] 4.1 Add `quota_set(tenant, ResourceClass::Process, ceiling)` to `author_enforcement` with a default ceiling of 100 and an operator-raise path; verify unit tests cover default, raised, and delinquent-zeroed ceilings
- [x] 4.2 Thread a `process_quota` value through the enforcement projection with a default of 100; verify the enforcement-projection tests assert the default and overrides

## 5. Bridge delegates the spawn bound

- [x] 5.1 Remove `SpawnBudget`, its constant, and the budget check from the bridge-server loop, and attach-then-close the stream when `Process::start_for_tenant` fails; verify the removed path no longer appears and a simulated denied spawn attach-then-closes the stream
- [x] 5.2 Replace the `spawn_budget_*` unit tests with tests asserting the denial path surfaces EOF to the connector peer; verify `cargo test -p selium-bridge` passes

## 6. Connector admission limits

- [x] 6.1 Set a per-connection bidirectional stream cap on the server QUIC transport config; verify with a connector test that streams beyond the cap are refused before a per-stream channel is allocated
- [x] 6.2 Add a token bucket keyed by authenticated tenant (falling back to the resolved serving tenant) in the connection accept loop, refilling on the host clock and resetting the stream with a distinct error code before `QuicChannel::allocate` when empty; verify with a unit test of bucket deprivation/refill and an integration test that a burst above the rate is refused while the connection stays up
- [x] 6.3 Evict idle rate-limit keys; verify with a unit test that an idle key is dropped and does not grow the bucket map unboundedly

## 7. Integration and validation

- [x] 7.1 Verify end-to-end that a completed bridge-channel pipe exits and its process is reaped (no zombie reactor), via the identity spine or a new integration test
- [x] 7.2 Verify end-to-end that exceeding a tenant's process ceiling is denied and surfaced to the external client as a clean stream close via the quota, with the bridge no longer keeping a spawn counter
- [x] 7.3 Run `cargo fmt --all`, `cargo clippy --workspace --all-targets -- -D warnings`, and `cargo test --workspace --all-targets` and resolve all failures
