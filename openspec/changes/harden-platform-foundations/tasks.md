# Tasks

## 1. Memory safety and hostile hostcall input

- [x] 1.1 Add misaligned public-atomic and heap-backing regressions, then implement aligned owned storage and effective-address checks for load/store/CAS/fetch-add/wait/notify; verify with `cargo test -p selium-memory` and a Miri run covering these APIs.
- [x] 1.2 Add hostile multi-memory count, overflow, out-of-range, and overlap fixtures, then validate table and entry geometry before allocation/sub-mapping; verify malformed inputs return `InvalidLayout` promptly without large allocations.
- [x] 1.3 Add a WASM hostile-envelope regression using oversized and out-of-range lengths, then enforce guest-memory bounds and `HOSTCALL_ENVELOPE_MAX_BYTES` before allocation/decode; verify the host survives with bounded memory and the golden spine still passes.
- [x] 1.4 Add `u64::MAX` and maximum-supported sleep tests, then use checked deadline arithmetic and typed rejection; verify no timer is allocated, no host panic occurs, and ordinary timer tests remain green.
- [x] 1.5 Add fuzz targets for hostcall envelopes, multi-memory headers, frame codecs, and module probing, with a short reproducible CI lane and corpus seeds; verify known malformed cases fail without panic, OOM, or undefined behaviour.

## 2. Scoped authority and key custody

- [x] 2.1 Add cross-tenant sign/revoke regressions and authorise every certificate operation against its requested tenant; verify an acme-scoped principal cannot alter beta and an authorised target still succeeds.
- [x] 2.2 Add root-plus-tenant and separate-grant delegation regressions, then evaluate each grant with full selector intersection; verify spawn and child-grant tests reject synthesised or foreign scope.
- [x] 2.3 Change first-boot persistence to retain root public material but not the root private key; verify a fresh state directory has no `root.key.der`, restart minting works, and identity certificate chains remain valid.
- [x] 2.4 Add validated legacy-keyring migration that removes online root private keys only after hierarchy validation; verify removal failure fails boot loudly, successful migration restarts without the key, and key-custody documentation matches the tests.

## 3. ABI compatibility and structured hostcall errors

- [x] 3.1 Introduce the fixed magic/version hostcall preamble and lockstep `SELIUM_ABI_VERSION` on host and guest; verify matching requests pass and missing, older, and newer versions fail before rkyv enum dispatch with both versions reported.
- [x] 3.2 Rebuild every in-tree guest and test fixture for the preamble, update module-loading documentation, and verify all WASM spines plus the no-WASI/import guards pass without mixed artifacts.
- [x] 3.3 Preserve `AbiError` code/message in `GuestError`, including immediate create failure, and poll/drop valid failed operations; verify matchable quota/permission/malformed errors and bounded operation counts after repeated denials.
- [x] 3.4 Add old-guest/new-host and new-guest/old-host fixtures and document the alpha lockstep compatibility policy; verify mismatches fail explicitly and same-version golden paths pass.

## 4. Wake, timer, and task correctness

- [x] 4.1 Add a deterministic enqueue-during-drain mailbox regression, then derive pending state from atomic head/tail rather than the advisory flag; verify no wake strands and an empty mailbox stalls without spinning.
- [x] 4.2 Add completion-between-check-and-registration coverage for `JoinHandle`, then register and re-check with correct ordering; verify repeated race runs always return or wake and never strand the joiner.
- [x] 4.3 Make guest timers round up positive sub-millisecond remainders, recheck deadlines after wakes, and propagate sleep errors through the revised timer result API; verify early/error completion tests and migrate all in-tree callers.
- [x] 4.4 Verify pending sleeps and hostcall futures are reclaimed on drop and process teardown, including the new error paths; verify timer and operation counters return to baseline in lifecycle and soak tests.

## 5. Cancellation-safe guest clients and RPC ownership

- [x] 5.1 Replace store/control manual gate/session checkout with RAII ownership guards; verify cancellation at each await point reopens or safely closes the client, permits subsequent requests, and leaves no stranded gate.
- [x] 5.2 Give RPC connect and accept establishment an owning guard from allocation through rendezvous; verify cancellation before and after handoff frees or closes regions exactly once and peers do not park indefinitely.
- [x] 5.3 Add repeated create/cancel/drop soak coverage for store, control, unary RPC, and streaming RPC; verify region, session, and host-operation counts remain bounded.

## 6. Runtime accounting, isolation, and shutdown

- [x] 6.1 Introduce rollback-on-drop quota/ownership reservation guards for region allocation, volume append, and queue send; verify injected resource failures restore headroom and ownership tables and successful operations commit once.
- [x] 6.2 Mark state-root preparation complete only after every fallible step succeeds; verify injected volume/keyring failures remain retryable and a corrected retry performs all incomplete work.
- [x] 6.3 Inject provider/context explicitly into runtime-owned channel use while retaining guest-local ambient installation; verify two coexisting runtimes with distinct kernels allocate and attach only through their own provider.
- [x] 6.4 Add idempotent bounded runtime shutdown and ownership for poller, timer, metering, and callback threads; verify repeated bootstrap/shutdown cycles leave no runtime-retaining threads or growing resource maps.
- [x] 6.5 Add lifecycle regressions for failed hostcalls, cancelled operations, guest-memory attachments, and teardown retry under the new guards; verify the resource-lifecycle scenarios currently uncovered by traceability have direct assertions.

## 7. Durable volumes, store recovery, and policy acknowledgement

- [x] 7.1 Share append synchronisation per underlying volume across handles and add concurrent two-handle tests; verify returned ranges never overlap, complete payloads read back in order, and store framing recovers interrupted writes.
- [x] 7.2 Invoke blob recovery before every first mutation, validate recovered content hashes, and avoid re-appending existing blobs; verify restart-then-put, torn append, duplicate put, manifest, and stable-slice tests.
- [x] 7.3 Make identity principal/grant/tenant/domain mutations durable before projection and acknowledgement; verify injected encode/append/sync failures preserve prior visible state and replay restores acknowledged state.
- [x] 7.4 Make accountant policy updates acknowledge only after durable commit and reconcile post-commit enforcement failures; verify ledger failure never returns `Updated` with uncommitted policy and restart converges committed intent.
- [x] 7.5 Audit control-plane and other durable producers against the same commit boundary, adding any missing failure propagation; verify component tests distinguish acknowledged from merely projected state.

## 8. Canonical rings, streams, and table authority

- [x] 8.1 Move frame sizing, reservation, encoding, decoding, and validation in `RingBuf` and channel wrappers behind `selium-shm::layout`; verify cross-wrapper conformance, oversized-frame rejection, buffer/header length checks, and corrected 16-byte header docs.
- [x] 8.2 Reuse shared byte-channel layout parsing and preserve independent inbound/outbound capacities; verify asymmetric ring attachment and normal symmetric streams.
- [x] 8.3 Implement incremental header/payload stream reads without a 64 KiB frame assumption; verify zero-length, 65,521-byte, and 1 MiB payloads plus fragmented reads preserve exact byte streams.
- [x] 8.4 Make `poll_shutdown` release writer registration, signal EOF, and reject later writes; verify TCP/QUIC byte streams drain committed bytes before peer EOF.
- [x] 8.5 Separate previous-flush completion from new-payload acceptance in `FramedWrite`; verify cancelled writes followed by different payloads neither lose nor falsely acknowledge data.
- [x] 8.6 Enforce single-writer authority for live-table CAS/version assignment and migrate in-tree publishers to explicit ownership; verify competing writers conflict deterministically and unarbitrated CAS is rejected loudly.
- [x] 8.7 Update channel and table API documentation with cancellation, shutdown, and single-authority contracts; verify doctest examples compile against the real behaviour.

## 9. Discovery and edge protocol convergence

- [x] 9.1 Update discovery replacement to maintain label and alias reverse indexes coherently; verify retargeted labels disappear and revoking an old target cannot revoke an alias now pointing elsewhere.
- [x] 9.2 Drive HTTP and QUIC resolver caches from route-watch replacement/revocation events, retaining failed-attach eviction; verify live target replacement and revocation converge without waiting for attach failure.
- [x] 9.3 Add bounded identity domain republishing from durable state; verify discovery restart while identity remains running restores every domain mapping.
- [x] 9.4 Replace HTTP parser `Option`/default-length conflation with incomplete, malformed, and oversized outcomes and consume all chunked trailers; verify invalid/conflicting lengths are rejected without request-boundary reuse and multi-trailer/pipelined cases pass.

## 10. Trustworthy verification and reproducible tooling

- [x] 10.1 Restrict traceability annotations to adjacent asserting tests, remove trailing declaration blocks, and add the 32 missing lifecycle/gate tests or specific reviewed waivers; verify `scripts/check-traceability.sh` reports zero uncovered scenarios and known detached annotations fail.
- [x] 10.2 Add an external HTTPS client → connector → real application guest spine covering routing, streaming, malformed input, and backpressure; verify the test fails when the external path is broken and attach its scenarios directly.
- [x] 10.3 Add a `sel` subprocess spine through the real connector, bridge, and control plane; verify request mapping, stdout/stderr, and exit codes, and remove annotation-only CLI coverage.
- [x] 10.4 Rename format-level crash tests accurately and add fault injection around actual store, identity, and control-plane append/sync/projection boundaries; verify restart replays exactly the state consistent with prior acknowledgements.
- [x] 10.5 Pin stable and dated nightly toolchains plus `wasm-tools`/`cargo-audit` versions in local and CI entry points; verify unchanged pins resolve reproducibly and an intentional bump updates all lanes.
- [x] 10.6 Make artifact guards honour `CARGO_TARGET_DIR` and add a non-default-target script regression; verify native/container gates inspect the artifacts Cargo actually built.
- [x] 10.7 Classify latency reports explicitly, add scheduled repeated flake discovery, and replace timestamp lock files/sub-second assertions with OS-held locks, bounded acquisition, and behavioural evidence; verify slow legitimate runs neither hang nor lose locks.
- [x] 10.8 Update the known-unknowns register for each remaining coverage limitation, including consumer write trust until its dedicated change lands; verify the gate prints accurate entries and no longer overstates platform, crash, or ABI coverage.

## 11. Maintainability and current-state reconciliation

- [x] 11.1 Split hostcall dispatch incrementally by resource family behind the existing dispatcher, preserving tests and public behaviour; verify no handler logic changes without its regression and `cargo clippy -p selium-runtime --all-targets -- -D warnings` passes.
- [x] 11.2 Re-export principal channel/pattern types from `selium-guest`, derive complete capability lists from one source, and remove confirmed dead error/ABI surface; verify the spine demo needs no direct lower-crate imports and capability coverage tests include every variant.
- [x] 11.3 Reconcile README, DESIGN-INTENT current-state text, runtime/network specs, discovery comments, contributor commands, and wasmtiny dependency instructions with executed behaviour; verify every documented command runs and scheduler deferral/consumer trust are explicit.
- [x] 11.4 Archive or supersede stale active changes and rebase remaining hardening/replication proposals onto current crates; verify `implement-system-guests`, `cluster-scaling`, and `hardened-consumer-shared-memory` no longer assert deleted paths or superseded behaviour.
- [x] 11.5 Add strict OpenSpec validation and stale-change checks to the gate; verify `openspec validate --strict` passes for this change and CI rejects invalid deltas or newly uncovered scenarios.

## 12. Consumer-tested release engineering

- [ ] 12.1 Add versions to internal publish dependencies and replace required repository-only patches with released upstream fixes or owned published packages; verify every `cargo publish --dry-run` succeeds from packaged sources without the workspace patch table.

  **Blocked on registry publication, not on code.** Internal dependencies already carry versions. The remaining repository-only patches are `wasmtiny`/`wasmtiny-aotc` (not on crates.io), the `web-time` fork (custom time source), and the `getrandom`/`ring` wasm patches. A packaged `cargo publish` resolves those from the registry, so this task cannot pass until the forks are published under Selium-owned packages; that needs registry credentials and a maintainer decision. `docs/release-process.md` records the exact enablement steps.
- [x] 12.2 Replace `--keep-going` publication with dependency-ordered, tag/version-validated automation gated on the full Linux gate; verify injected dependency failure stops dependants and marks the release incomplete.
- [x] 12.3 Package `sel`, the runtime, matching system-guest WASM, and code-defined bootstrap assets, or label component-only artifacts explicitly; verify the packaged smoke path requires no source-checkout paths and its documentation covers state/key requirements.
- [x] 12.4 Add startup and guest-bootstrap smoke tests for every advertised OS/architecture artifact; verify a failed target blocks its support claim while other targets remain accurately reported.
- [ ] 12.5 Add an isolated downstream consumer that builds a no-WASI guest and instantiates it against the matching published host; verify ABI negotiation, golden-path logging, and unsupported mixed-version failure without repository patches.

  **Blocked on 12.1.** "Published host" means crates.io packages; with the engine forks unpublished the consumer can only use path dependencies, which does not exercise the downstream packaging path. ABI negotiation and the mixed-version refusal are already covered in-tree (`hostcall_envelope` and the guest preamble tests).
- [ ] 12.6 Keep crates.io publication disabled until tasks 12.1–12.5 pass, then document the lockstep compatibility/release process; verify a rehearsal tag produces only reviewed, complete artifacts.

  **Partially done, deliberately still open.** Publication stays disabled (`if: false` in `.github/workflows/publish.yml`) and `scripts/publish.sh` refuses without a gate marker. The lockstep process is documented in `docs/release-process.md`. Enabling the lane and verifying a rehearsal tag require 12.1/12.5 first.

## 13. Final integration acceptance

- [x] 13.1 Run `cargo fmt --all --check`, strict workspace Clippy, workspace tests/doc tests, all WASM prerequisite builds, patch/shared-memory guards, and strict OpenSpec/traceability checks; verify every result is green and recorded in the change.
- [x] 13.2 Run the complete ignored integration pack, real HTTPS/CLI spines, stage-2 wait-word suites, and repeated create/shutdown/cancellation lanes on Linux; verify no ignored required test, detached annotation, flake, or cross-runtime interference remains.

  **Recorded result.** `scripts/gate.sh` on Linux: every step passed —
  formatting, check, clippy, workspace and doc tests, WASM prerequisites, the
  atomics shared-memory declaration, the wasm-patch guard, traceability,
  OpenSpec/stale-change validation, the release-tooling smoke, the stage-2
  wait-word suites, and the full ignored pack (`flake-check: run 1 passed,
  no flakes`), including the real HTTPS and CLI spines. The container's
  datagram-drop stalls were diagnosed to the kernel poller draining the OS
  socket into a full rewrite ring; it now defers to the kernel buffer and
  paces the retry. No detached annotations remain.

## 13.1 evidence (recorded)

- `cargo fmt --all --check` — clean.
- `cargo clippy --workspace --all-targets -- -D warnings` — clean.
- `cargo test --workspace --lib` and the affected integration crates — green.
- `scripts/build-all.sh` — all WASM prerequisites built.
- `scripts/check-wasm-patches.sh` — ok.
- `scripts/check-traceability.sh` — 962 scenarios (880 shipped + 82 this change's delta ids), 819 adjacent annotations, 143 scenario-specific reviewed waivers, 0 uncovered, 0 detached; `--self-test` pins the classifier. The checker treats every unparked active change's deltas as required, so this change's own scenarios are covered *before* the sync step at archive rather than after it.
- `scripts/check-openspec.sh` — every unparked active change is strict-valid and no active change asserts a removed path.
- Golden `spine` test — pass; the real `identity_spine` (CLI subprocess), `no_mtls_spine`, `quic_spine`, and `https_spine` (external TLS client, streaming, 404/413, missing-TLS refusal) pass.
- `cargo audit -D warnings -D unmaintained -D unsound -D yanked` — clean after `rustls 0.23.46`, `rustls-webpki 0.103.16`, `tokio 1.53.3`.
- `scripts/fuzz-smoke.sh` — every target passes; `cargo +nightly miri test -p selium-memory --lib` — 32 tests pass.
- Release tooling — `publish.sh --self-test` and the packaged-artifact smoke (`--version` + real guest bootstrap) are part of the gate.
- Toolchain pinning — `rust-toolchain.toml`, every workflow, and the local
  container entry point (`scripts/gate.sh`: `rust:1.99.1`, nightly
  `nightly-2026-10-25`, wasm-tools `1.259.0`, OpenSpec `1.16.0`) name the same
  versions, so a local gate and CI resolve the same tools; `publish.yml`'s
  artifact matrix installs the cross target *and* `wasm32-unknown-unknown` for
  the pinned toolchain the checkout actually selects.
- Durable acknowledgement — `StoreRequest::Sync` is the explicit barrier the
  identity, accountant, and control-plane policy paths call after their append
  and before projecting or acknowledging; the store still owns batching for
  high-volume bookkeeping. A failed sync is asserted at the store
  (`injected_sync_failure_is_reported_for_an_appended_record`).
- Crash lane — the format-level tests are renamed `format_*` and
  `scripts/flake-check.sh` skips them by their new names, so the merge gate
  excludes the nightly crash lane again (they were silently re-included after
  the rename).

## 14. Verification remediation

- [x] 14.1 Route every runtime-owned RPC close through its explicit provider and reject conflicting guest provider initialisation; verify second-runtime teardown and conflicting-init regressions.
- [x] 14.2 Make runtime shutdown drop-safe, bounded, and inclusive of live guest workers; verify drop, worker-pool, and wedged-shutdown tests leave no runtime-retaining activity.
- [x] 14.3 Bound hostile hostcall operation state and preserve operation IDs end to end; verify non-cooperative malformed requests stay bounded and rollover cannot strand operations.
- [x] 14.4 Make ABI incompatibility fail at guest bootstrap before request dispatch; verify legacy, older, newer, and matching module tests.
- [x] 14.5 Reject partial keyring hierarchies and verify the root-to-intermediate relationship before legacy-key removal; verify partial and unrelated-valid-CA fixtures.
- [x] 14.6 Own RPC regions from allocation through client/server establishment and close every early failure path; verify injected failures and cancellation leak no region or parked peer.
- [x] 14.7 Bind live-table write authority to actual shared channel identity and arbitrate competing writers; verify local alias and cross-guest CAS conflicts.
- [x] 14.8 Make blob put/retry and torn recovery preserve validated content and manifest coherence; verify sync-failure retry, valid-prefix preservation, and no duplicate payload.
- [x] 14.9 Serialise identity/accountant commit projection order and reconcile CA/grant enforcement failures before acknowledgement; verify concurrent mutations and restart replay.
- [x] 14.10 Preserve structured SDK errors, finish canonical ring delegation, propagate shutdown failures, and strictly parse HTTP transfer codings; verify direct failure-path regressions.
- [x] 14.11 Remove truncating guest `Instant` duration conversions; verify durations above `u64::MAX` nanoseconds overflow instead of moving deadlines.
- [x] 14.12 Recover connector watches and rediscover replacement discovery listeners; verify watch-outage route convergence and actual discovery replacement domain replay.
- [x] 14.13 Replace predicate/helper-only evidence with deterministic mailbox, join, timer, zero-frame, and asymmetric-attach behaviour tests; verify each named scenario directly.
- [x] 14.14 Extend HTTPS/CLI spines and add genuine store/identity/control-plane persistence interruption and restart tests; verify malformed framing, backpressure, success/error output, and acknowledgement-consistent replay.
- [x] 14.15 Finish toolchain/flake/main-spec/CLI documentation reconciliation; verify all local pins, cached-version checks, behavioural timing assertions, and current-state claims.
- [x] 14.16 Generate publication order from Cargo dependencies, isolate artifact smoke from the source tree, and stage uploads until all targets pass; verify publication-plan and release-workflow self-tests.
- [x] 14.17 Run focused remedial regressions, strict OpenSpec/traceability checks, formatting, Clippy, workspace/doc tests, WASM spines, and the full Linux gate; record every result without claiming unrun lanes.

### Remediation verification

- Full Linux `scripts/gate.sh`: **PASS** (1031 s), including workspace/doc tests,
  strict Clippy, formatting, WASM prerequisites and guards, OpenSpec and
  traceability checks, release tooling/artifact smoke, stage-2 wait-word
  conformance, and the complete ignored pack with no flakes.
- The ignored pack passed HTTPS streaming/backpressure, CLI success/error,
  identity/control-plane durability, discovery replacement, QUIC echo,
  multithreaded SDK execution, and every failure-injection spine.
- Native focused reruns also pass identity/CLI (**258 s**) and QUIC (**54 s**).
- `git diff --check` and `cargo fmt --all --check`: clean.
