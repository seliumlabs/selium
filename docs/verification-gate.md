# The verification gate

`scripts/gate.sh` is the single reproducible entry point for how this project
verifies itself. CI and a developer run the same command.

```sh
scripts/gate.sh                 # the full gate
scripts/gate.sh --quick         # everything except the ignored pack
scripts/gate.sh --soak          # the full gate plus soak/crash lanes
scripts/gate.sh --soak-only     # just the soak/crash lanes (nightly)
scripts/gate.sh --repeat 3      # run the ignored pack 3 times, flag flakes
scripts/gate.sh --native        # host subset; do not re-exec into Linux
```

On a non-Linux host the script re-execs itself inside the Linux container that
`scripts/test.sh` and `scripts/clippy.sh` already use, because parts of the
workspace only compile or behave on Linux. `--native` opts out for a quick
subset; that run says which Linux-only steps it skipped.

The container run sets `CARGO_PROFILE_DEV_DEBUG=0`: Docker's VM disk is shared
by every build volume, and full debuginfo roughly triples the artifact tree
(sufficient to fill a 60 GiB disk). The code, profiles, and tests are identical;
only backtrace line information is trimmed. CI, with its own disk, keeps the
default debuginfo.

The container pins the same tools as CI: the base image is `rust:1.99.1`, the
nightly toolchain is `nightly-2026-10-25` (`SELIUM_NIGHTLY_TOOLCHAIN`),
wasm-tools is `1.259.0` (`WASM_TOOLS_VERSION`), and OpenSpec is `1.16.0`
(`OPENSPEC_VERSION`). `scripts/test.sh`, `scripts/clippy.sh`, and
`scripts/build-all.sh` use the same stable and dated-nightly defaults; the gate
also rejects a cached `wasm-tools` binary whose `--version` differs from the pin
rather than silently using it. A developer's gate and the merge gate resolve
identical toolchains and utilities.

## What the gate runs

| Step | Command |
| --- | --- |
| Formatting | `cargo fmt --all --check` |
| Type check | `cargo check --workspace --all-targets` |
| Lints | `cargo clippy --workspace --all-targets -- -D warnings` |
| Workspace tests | `cargo test --workspace --all-targets` |
| Doc tests | `cargo test --workspace --doc` |
| Wasm prerequisites | `scripts/build-all.sh` (plain guests, nightly atomics net-demo, mt-demo) |
| Atomics shared memory | `wasm-tools print .../selium_net_demo_atomics.wasm` must declare shared memory |
| Wasm patch guard | `scripts/check-wasm-patches.sh` |
| Spec traceability | `scripts/check-traceability.sh` (adjacency-enforcing, shipped specs **plus** every unparked change's deltas; `--self-test` pins the classifier) |
| OpenSpec and stale changes | `scripts/check-openspec.sh` (strict delta validation for every unparked active change; removed-path rejection) |
| Stage-2 wait words | `wait_notify` conformance plus kernel/runtime suites with `stage2-wait-words` |
| Ignored pack | `scripts/flake-check.sh` (full `--ignored` pack, retried) |
| Failure injection | the `failure_*` binaries, run by the ignored pack: authority unavailable, discovery unavailable, partial teardown/retry, crash vs graceful exit, lease expiry under load, claim collision/forgery, cross-process wake races |
| Soak/crash (`--soak`) | `soak` and `crash_consistency` test binaries |
| Result | the open known-unknowns register is printed with the outcome |

Every step runs even when an earlier one fails; the gate exits non-zero if any
step failed, so one run reports every regression it can. A green gate requires
every component to pass — there is no selected subset.

## Required check

The CI workflow's `Gate` job is the aggregate required check. "Required" is a
repository branch-protection setting, not a workflow property: in
**Settings → Branches → Branch protection rules**, the default branch must
require the `Gate` status check. The former `Cargo checks`, `Cargo tests`,
`Golden-path spine (WASM guest)`, `Shared-page fast path (atomics guest)`, and
`Shared-page fast path Stage 2` jobs were removed because the gate subsumes
them; leaving them required would merely re-run part of the gate.

`Soak and crash-consistency` is not a merge requirement: it runs nightly and on
demand. Its results are reported in the workflow run, which is where a
long-running regression becomes visible.

## Scheduled and auxiliary lanes

These lanes run outside the merge gate but are part of the verification story;
each pinned tool version is checked in.

| Lane | Command | Pin |
| --- | --- | --- |
| Security audit | `cargo audit -D warnings -D unmaintained -D unsound -D yanked` | `cargo-audit 0.22.2` |
| Fuzz smoke | `scripts/fuzz-smoke.sh` (short bounded run over the checked-in corpus) | `cargo-fuzz 0.13.2`, `nightly-2026-10-25` (the script checks both) |
| Soak and crash | `scripts/gate.sh --soak` (`soak`, `crash_consistency`) | — |
| Miri | `rustup component add miri --toolchain nightly-2026-10-25 && cargo +nightly-2026-10-25 miri test -p selium-memory --lib` | dated nightly + `miri` component |

The audit lane is strict: an advisory that cannot be fixed must be a written,
reviewed entry in `docs/known-unknowns.md`, not a silent ignore. Dependencies
are kept on patched releases (`rustls >= 0.23.46`, `rustls-webpki >= 0.103.16`,
`tokio >= 1.53.3` at the time of writing).

Miri covers the unsafe-substrate crate (`selium-memory`: pointer backend,
alignment, atomics — 32 tests). `selium-shm`'s unit suite is not run under Miri:
its stress/atomic scenarios exceed a 40-minute bounded attempt, so the native
suite (including the Miri-friendly memory layer underneath it) is the evidence
there. This is recorded rather than implied.

## Flake detection

`scripts/flake-check.sh` runs the ignored pack and either retries only the
tests that failed (default) or runs the whole pack `N` times (`--runs N`). A
test that only passes on retry, or that passes in some runs and fails in
others, is reported as **FLAKY** and fails the gate. A test that fails every
run is reported as failing, not flaky. The gate cannot go green on a retry.

### Why not `cargo-nextest`?

Evaluated for sharding and retries. The retry-with-flag requirement is met by
`scripts/flake-check.sh` without adding a second runner, a second output format,
or a second configuration surface, and the single Linux job is within the
merge-gate time budget. `cargo-nextest` remains a candidate if the pack later
needs sharding across several CI runners; nothing in the current wrapper
prevents adopting it then. The point is recorded here so the decision is not
re-litigated from memory.

## Load-sensitive tests: outcomes

Three tests were observed failing under load. Each outcome is recorded here so
the next flake report can be triaged against a known baseline.

| Test | Cause | Outcome |
| --- | --- | --- |
| `multithreaded` (`two_cpu_bound_tasks_beat_serial_wall_time_on_two_workers`) | asserted `parallel < serial × 0.8` on wall-clock time; a loaded or busy host inverts or narrows the ratio, and two workers can contend on one core | **Fixed.** Replaced with `two_workers_reach_a_shared_rendezvous_concurrently`: both workers must arrive at a two-party atomic rendezvous. A serial or single-worker run cannot complete at all, so the result is load-independent; the bound only separates concurrent from deadlocked. The settle deadline in `aggregate_observation` likewise now fails loudly at `WORKER_SETTLE_BOUND` instead of asserting on half-settled data. |
| `quic_spine` | interpreted TLS handshake and paced relay take minutes; the timeouts were incidental literals, so a loaded runner could exceed them with no reviewable reason | **Bounded.** Named reviewed constants (`HANDSHAKE_BOUND` 240 s, `RELAY_BOUND` 300 s, `OBSERVATION_BOUND` 60 s), each documented. No behavioural change: the relay is paced by quinn, not by the bound. |
| `identity_spine` | handshake, anchor-set rebuild, and revocation refusal waited on the same incidental literals; the spine also flaked while the interpreted connector's 64 KiB UDP rewrite ring dropped handshake datagrams under load | **Bounded and diagnosed.** Uses the shared harness constants (`HANDSHAKE_BOUND`, `CONVERGENCE_BOUND`, `OBSERVATION_BOUND`). The kernel poller's UDP rewrite ring is now 1 MiB, absorbing the handshake burst instead of dropping it; three consecutive spine runs pass. A longer burst remains a documented timing limit (see `docs/known-unknowns.md`). |
| `stress::n_writers_single_reader_no_lost_wakeups` | a 10 s completion deadline measured on a host running the whole workspace suite in parallel | **Bounded.** 120 s deadline; the failure this distinguishes is a lost wake (never completes), not a slow one. |

Re-run evidence for these results is in the change's task record; a flake that
recurs now points at the test's behaviour rather than at an unstated timeout.


## Verification record (this change)

The gate was run on macOS natively (`scripts/gate.sh --native --repeat 2`)
and the ignored pack was also run per binary while triaging. Recorded outcome:

- formatting, `cargo check`, clippy, workspace tests, doc tests, wasm
  prerequisites, the atomics shared-memory declaration, the wasm-patch guard,
  and spec traceability pass. (One full run's formatting step failed because
  the annotation sweep was being formatted concurrently; a re-run is clean.)
- the failure-injection suites pass 7/7, the soak's bounded-state assertion
  passes (including the departed-client session release), and the crash lane
  passes 3/3.
- the quick aggregate gate passes end to end after the RPC-session fix
  (check, clippy, workspace tests, doc tests, wasm prerequisites, the atomics
  declaration, the wasm-patch guard, and traceability).
- the full ignored pack's `power_cycle` failure was triaged and fixed (task
  8.6). The quota leg completed; under pack load it finished before the test
  observed its log channel, and the reap path had already drained the frames
  into the `ProcessExited` activity record, so `wait_for_logs` reported an
  empty log and the leg looked hung. Both log waiters now also read the
  reap-left activity summary; two concurrent `power_cycle` runs (the load that
  reproduced it) pass. The dead-sender session the first diagnosis blamed is
  the lease-renewal task's second discovery session: a short-lived guest exits
  before discovery accepts it, its region is freed, and the delivered attach is
  denied. That is a benign stale handoff, not a live client's hang, and no
  transfer change can attach a destroyed region.
- the same run surfaced and fixed two more test-observation defects: the pack's
  `--skip crash_` filter also excluded `failure_crash_vs_graceful` (now only
  the crash lane's own three names are skipped, and the failure test runs), and
  `accountant_recovery` asserted an intermediate enforcement state (it now
  waits, bounded, for every asserted quota class). `dns_spine` failed that run
  with only `dns-demo: booting` observed: its local waiter had the same
  post-exit log-loss race, now fixed; the `identity_spine`/`quic_spine` bounds
  held in this run and remain reviewed.
- the full ignored pack was re-run after those fixes
  (`scripts/flake-check.sh --runs 1`): every binary passed, with no failure or
  flake, including `power_cycle` (twice concurrently before the pack),
  `dns_spine`, `accountant_recovery`, and `failure_crash_vs_graceful` (now
  enforced rather than skipped).
- known-bad input was proved to fail the gate: a deliberately broken ignored
  spine, a clippy warning, an unformatted file, a missing waiver, and a ring
  patch resolved from crates.io each fail their step.

## Verification record (sections 10-13)

- **Full Linux gate: PASS.** Every step passed — formatting, check, clippy,
  workspace and doc tests, WASM prerequisites, the atomics shared-memory
  declaration, the wasm-patch guard, traceability, OpenSpec/stale-change
  validation, the release-tooling smoke (`publish.sh --self-test` plus
  `smoke-artifact.sh` starting the runtime and bootstrapping the golden-path
  guest), the stage-2 wait-word suites, and the full ignored pack
  (`flake-check: run 1 passed, no flakes`), including the real HTTPS and CLI
  spines.
- The Linux container's QUIC/TLS stalls were diagnosed and fixed: when the
  shared rewrite ring filled, the kernel poller drained the OS socket and
  dropped every datagram it read. It now stops consuming the socket, paces a
  1 ms retry, and leaves the burst to the kernel buffer; the rewrite ring is
  1 MiB and the connector's reviewed idle/handshake budgets are 300 s/420 s.
  The identity spine now passes in the container, and the pack is green.
- Security audit is clean after moving to patched releases: four `rustls`
  advisories and one `rustls-webpki` advisory were fixed by `0.23.46` and
  `0.103.16`, and the `tokio` `AsyncFd` unsound advisory by `1.53.3`;
  `cargo audit -D warnings -D unmaintained -D unsound -D yanked` exits zero.
- `scripts/fuzz-smoke.sh` passes every target (~860k executions in the 15 s
  budget per target).
- `cargo +nightly-2026-10-25 miri test -p selium-memory --lib` passes 32 tests.
- The identity spine's repeated flakiness was diagnosed to the kernel poller's
  64 KiB UDP rewrite ring dropping handshake datagrams under load; the ring is
  now 1 MiB and the spine passes repeatedly.
- The CPU-bound multithreaded demo guest is interpreted, so its workload was
  reduced (`1 << 16` iterations per task): the end-to-end lane still proves a
  clean concurrent exit, while the deterministic wall-clock concurrency factor
  stays in the native wat fixture.

## Spec-scenario traceability

Every scenario in `openspec/specs/**/spec.md` is either associated with a test
or explicitly waived. The deltas of every **unparked** active change under
`openspec/changes/` are held to the same rule: a change's new scenarios need
their annotation or waiver before the change is complete, so "gate green" does
not depend on the deltas still being unsynced. A change carrying `PARKED.md`
(explicitly outside the release gate, per `scripts/check-openspec.sh`) is not
scanned.

### Annotation

Put the comment line directly above the test that actually asserts the
scenario. An annotation that is not attached to a test (a trailing declaration
block, a header comment, an annotation above a helper) is rejected and fails
the gate; `scripts/check-traceability.sh --self-test` pins that classifier with
checked-in fixtures. Scenarios the workspace deliberately does not implement
carry a specific reviewed waiver in `openspec/traceability-waivers.md`.

```rust
// scenario: discovery-registration/host-authoritative-admission/authority-failure-fails-closed
#[test]
fn an_authority_read_failure_fails_closed() { ... }
```

The id is the capability directory, then the requirement and scenario headings
slugged: lowercase, every run of non-alphanumeric characters becomes a single
`-`, leading/trailing `-` trimmed. A test may carry several annotations; one
scenario may be claimed by several tests.

### Waiver

`openspec/traceability-waivers.md` is a Markdown table of reviewed waivers:

```markdown
| Scenario | Reviewer | Reason |
| --- | --- | --- |
| capability/requirement/scenario | @reviewer | needs hardware this project does not run |
```

A waiver with no reason fails the check, and so does an annotation or waiver
naming a scenario that no longer exists (drift). Each row states why *that*
scenario cannot be tested here; a reason shared verbatim across several rows is
a blanket waiver and is treated as review debt, not evidence. A waiver whose
premise has been met (for example "until the spine lands" after it has landed)
must be replaced with an annotation.

`scripts/check-traceability.sh --report` prints the uncovered scenarios per
capability without failing, which is the adoption view; the gate runs it
strictly.

An annotation naming a scenario that exists only in an unarchived change is
accepted: the checker treats an unparked change's delta ids as known *and*
required. Write the annotation next to the asserting test as the implementation
lands; the id keeps counting after the sync step at archive. `--report` remains
the non-failing view for adoption.

## Latency assertions are behavioural bounds, not performance SLOs

The spine and timer tests use wall-clock bounds only to distinguish "the
eventual wake happened" from "it never happened" (for example a convergence
bound or a timer's at-or-after-deadline guarantee). They are not latency
reports and do not measure throughput. Bounds are deliberately generous so a
loaded CI runner fails only on a missing event, not on scheduling jitter; a
sub-second assertion that could fire under normal load is treated as a flake,
not as evidence. Convergence and liveness are asserted behaviourally wherever a
wall-clock reading is not itself the contract.

`#[ignore]` classifies execution prerequisites, not evidence quality: the
ignored pack needs prebuilt WASM guests, fixed ports, or other integration
setup and is run explicitly by `flake-check.sh`; only the soak and `format_*`
crash lanes are reserved for the scheduled lane. The kernel's separately named
`*_latency_report` tests are explicitly non-gating measurements, are outside
the runtime ignored pack, and run only on request with `--nocapture`; they have
no pass threshold. None of the runtime integration tests is a latency
benchmark. A removed sub-second assertion is replaced by the guest's
own wake/echo/rendezvous behaviour, while its generous timeout remains only a
missing-event backstop. Actual timer deadline tests are the exception because
time is the contract under test.

Repeated flake discovery runs on the scheduled lane (`flake-check.sh --runs 3`)
so a test that only fails under repeated load is classified as flaky rather
than merged from a single green run.
