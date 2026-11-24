# verification-gate Specification

## Purpose

The enforced, reproducible contract for how the project verifies itself: what the
gate must run, on which platform, with what failure-injection, traceability,
determinism, and soak coverage, and precisely what a green gate asserts.

## Requirements

### Requirement: Full ignored pack in CI
The project SHALL run every `#[ignore]`d integration test as part of an enforced
continuous-integration job on Linux, except tests explicitly classified as
non-gating latency reports. It SHALL build every prerequisite the pack needs —
including the nightly atomics and multithreaded guest artifacts — before the
tests run. A selected-path job SHALL NOT stand in for the pack.

#### Scenario: A broken ignored spine fails CI
- **WHEN** a pull request breaks an ignored integration test such as `control_plane` or `identity_spine`
- **THEN** the continuous-integration run SHALL fail

#### Scenario: Prerequisites are built
- **WHEN** the gate runs
- **THEN** it SHALL build the wasm guests, the atomics net-demo, and the mt-demo artifact before running the tests that need them

#### Scenario: The gate runs on Linux
- **WHEN** the gate runs in CI
- **THEN** it SHALL run on the Linux environment the project targets, not a developer host

### Requirement: Whole-workspace gate
The gate SHALL include the workspace unit and integration tests, doc tests,
`cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --check`, the
wasm guest builds, and the wasm-patch guard. A green gate SHALL require every
component to pass, not a selected subset.

#### Scenario: A non-spine crate regression fails CI
- **WHEN** a change breaks a test outside the runtime integration spines
- **THEN** the gate SHALL fail

#### Scenario: A wasm patch regression fails CI
- **WHEN** the ring/getrandom wasm patches silently fall back to the JS-backed RNG
- **THEN** the wasm-patch guard SHALL fail the gate

#### Scenario: Formatting and lints are enforced
- **WHEN** a change is unformatted or introduces a clippy warning
- **THEN** the gate SHALL fail

### Requirement: Failure-injection coverage
The pack SHALL include tests for the named failure modes: host authority
unavailable at admission, discovery unavailable, partial teardown and retry,
crash versus graceful exit, lease expiry under load, claim collision and forgery,
and cross-process wake races. Each SHALL assert the documented fail-closed or
convergent outcome.

#### Scenario: Authority failure fails closed
- **WHEN** the host authority cannot answer an admission query
- **THEN** a test SHALL assert that discovery refuses the registration rather than falling back to the projection

#### Scenario: Discovery outage does not hang a client
- **WHEN** discovery is unavailable to a connecting guest
- **THEN** a test SHALL assert the client parks or errors within a bounded time rather than spinning or hanging

#### Scenario: Partial teardown is retryable
- **WHEN** a revocation publish fails mid-teardown
- **THEN** a test SHALL assert the stop reports failure and a later retry completes the remaining revocations

#### Scenario: A forged claim is refused
- **WHEN** a guest without a claim publishes a reserved name
- **THEN** a test SHALL assert the registration is refused

### Requirement: Pack exclusions are explicit and narrow
The pack's lane exclusions SHALL name only the tests that belong to the
long-running soak and crash-consistency lanes. A broad prefix SHALL NOT exclude
a failure-injection test, and every failure mode named in this spec SHALL run
in the enforced pack.

#### Scenario: Crash-versus-graceful failure test runs in the pack
- **WHEN** the pack excludes the nightly crash-consistency lane
- **THEN** `failure_crash_vs_graceful` SHALL still run in the pack

### Requirement: Post-exit milestones are observable
A milestone a guest logged before it exited SHALL remain observable to a test
waiter, including when the guest completed before the waiter started. The
reap path SHALL retain the drained lines in the process's activity record, and
the wait helpers SHALL read that record as well as the live log channel.

#### Scenario: One-shot guest completes before its wait
- **WHEN** a short-lived guest logs a milestone and exits before the test waits
- **THEN** the wait SHALL succeed from the activity record rather than reporting an empty log

### Requirement: Spec-scenario traceability
Every OpenSpec requirement scenario SHALL be associated with at least one test or
an explicit, reviewed waiver. The gate SHALL report requirement scenarios that
have neither, so an untested requirement is visible rather than implied.

#### Scenario: Untested requirement is reported
- **WHEN** a requirement scenario has no associated test and no waiver
- **THEN** the traceability check SHALL report it

#### Scenario: Waiver is explicit
- **WHEN** a scenario is intentionally untested (for example, it requires hardware the project does not run)
- **THEN** a reviewed waiver SHALL record the reason

### Requirement: Determinism and flake detection
The gate SHALL detect flaky tests rather than trusting a single run, and tests
SHALL NOT depend on host load or wall-clock timing beyond declared bounds.

#### Scenario: A flaky test is flagged
- **WHEN** a test passes and fails across repeated runs of the gate
- **THEN** the gate SHALL flag it as flaky rather than reporting green

#### Scenario: Timing bounds are declared
- **WHEN** a test waits on convergence
- **THEN** its timeout SHALL be an explicit, reviewed bound rather than an incidental default

### Requirement: Soak and crash-consistency coverage
The pack SHALL include long-run checks for host-held resources and restart
checks at more than one point, beyond a single clean power cycle.

#### Scenario: Resource growth is detected
- **WHEN** a soak run exercises many spawns, timers, and registrations
- **THEN** a test SHALL assert that host-held resources (timer driver, discovery and claim tables, region and queue ownership) do not grow without bound

#### Scenario: Restart mid-write recovers
- **WHEN** a durable component is stopped during a write and restarted
- **THEN** a test SHALL assert the durable state replays to a consistent state

### Requirement: Known-unknowns register
The project SHALL maintain a register of known-unknowns — behaviours the gate
does not exercise — and the gate SHALL report it, so a green run is qualified by
what it does not cover.

#### Scenario: Green gate is qualified
- **WHEN** the gate passes
- **THEN** it SHALL report the open known-unknowns alongside the result

#### Scenario: Register stays current
- **WHEN** a new gap is identified
- **THEN** it SHALL be recorded in the register rather than left implicit
