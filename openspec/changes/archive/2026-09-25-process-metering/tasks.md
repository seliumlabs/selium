# Tasks

Implementation assumes the Wasmtiny `instance-metering` change lands first, since several tasks consume its stats/budget API.

## 1. ABI: instruction unit of account

- [x] 1.1 Replace `cpu_micros` with `cpu_instructions: u64` in `selium_abi::MeteringObservation` (struct, rkyv encoding, every constructor/reader); verify `cargo test -p selium-abi` passes and no `cpu_micros` reference remains under `crates/abi`
- [x] 1.2 Update the observation codec tests to assert the `cpu_instructions` field round-trips and the `cpu_micros` field is absent; verify `cargo test -p selium-abi` covers both scenarios

## 2. Downstream type regeneration

- [x] 2.1 Regenerate the FlatBuffers accountant tables (`MeteringBucket`, `TenantPlan`) so CPU fields carry instructions and update the `selium_service` builder/accessor glue; verify `cargo build -p selium-service` and its unit tests pass
- [x] 2.2 Update `guests/accountant` `Usage`, `Account`, cumulative-counter, and reduction logic to the instruction unit; verify `cargo test -p selium-accountant` passes

## 3. Runtime projection from the engine

- [x] 3.1 Read each process's executed-instruction count from the Wasmtiny instance after every reactor poll and accumulate it into `MeteringProjector::cpu`, so production always feeds the accumulator from engine observations; verify a unit test that drives a guest poll and asserts the projected `cpu_instructions` increases
- [x] 3.2 Source the per-process memory gauge from committed linear-memory pages (Wasmtiny owned-page count) instead of shared-region ownership; verify the metering-tick unit test asserts linear pages, not region bytes
- [x] 3.3 Retire the manual `record_cpu_usage` placeholder path and update `metering_tick_projects_counters_and_gauges` and `accountant_spine` tests to the engine-fed model; verify `cargo test -p selium-runtime` passes
- [x] 3.4 Aggregate instruction and memory readings across a multithreaded guest's worker threads into one per-process observation (carried over from `multithreaded-guest-execution`, which removed its own aggregation task because the metering model is defined here); verify a multithreaded guest consuming CPU on several workers reports one summed per-process observation

## 4. CPU budget enforcement

- [x] 4.1 Accept each tenant's per-minute CPU instruction ceiling into the runtime (authored under a `ResourceClass::Cpu` quota) and translate it into a per-process engine execution budget, re-applied on each sampling tick (so a ceiling change lands within one second) with the budget window re-anchored on each wall-clock minute boundary; verify a unit test that changes the ceiling and observes the engine budget change on the next refresh and a unit test that the window re-anchors only on a new minute
- [x] 4.2 On engine budget exhaustion, record `ProcessExited` and tear the process down through `cleanup_failed_process` (regions, pipe slots, process-quota slot released); verify an E2E test that a runaway-loop guest is reaped and its reservations are released
- [x] 4.3 Keep the exhaustion outcome a first-class signal in the runtime: the engine returns the distinct budget-exhausted trap, the runtime owns the reap decision; verify by asserting the engine reports `ExecutionBudgetExceeded` and no teardown logic was added to the engine
- [x] 4.4 Anchor each process's engine execution budget from spawn so a process spawned mid-window is bounded from its first instructions; verify a unit test that spawns under an already-authored ceiling and is reaped on its first poll without a budget refresh
- [x] 4.5 Align the process budget window to the accountant's wall-clock minute boundaries; verify a unit test that the window index equals the current UNIX minute

## 5. Accountant authoring

- [x] 5.1 Author each good-standing tenant's per-minute CPU ceiling as plan + overage, converted to instructions at the pricing-boundary rate card, and republish it each minute; verify an accountant unit test asserting the authored ceiling equals plan + overage
- [x] 5.2 Zero a delinquent tenant's CPU ceiling alongside its other quotas; verify the existing delinquent-transition test asserts a zero CPU ceiling

## 6. Cross-cutting verification

- [x] 6.1 Update every test and assertion referencing `cpu_micros` (runtime, accountant, service, guest metering read API) to `cpu_instructions`; verify `cargo test --workspace --all-targets` passes
- [x] 6.2 Run `cargo fmt --all`, `cargo clippy --workspace --all-targets -- -D warnings`, and `cargo build --target wasm32-unknown-unknown -p selium-spine-demo -p selium-discovery`; verify clean output
- [x] 6.3 Run the golden-path spine test and verify metering observations flow end-to-end without the placeholder hook; verify `cargo test -p selium-runtime --test spine -- --ignored` passes
