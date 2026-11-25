# Fuzzing failure playbook

This is the procedure for turning a failing fuzz run into a reviewed fix and a
regression seed. It assumes no prior fuzzing experience. The short version:

1. Download the crash artifact.
2. Reproduce it locally; it must still fail.
3. Minimise that one input with `cargo fuzz tmin`.
4. Find the owning code, fix the cause, and add a regression test.
5. Replay the minimised input; it must now pass.
6. Add it as a named seed and run the smoke lane.
7. Commit, push, and confirm CI.
8. Only after green, download the corpus and refresh the seeds with `cmin`.

Steps 3 and 8 use different tools. `tmin` shrinks one crashing input; `cmin`
shrinks a whole corpus. Do not substitute one for the other.

## 1. Vocabulary

| Term | Meaning |
| --- | --- |
| **Fuzz target** | The Rust function that accepts arbitrary bytes: `fuzz/fuzz_targets/<target>.rs`. |
| **Input** / **test case** | One byte string passed to the target. |
| **Seed** | A reviewed starting input committed under `fuzz/seeds/<target>/`. |
| **Working corpus** | Inputs a run has discovered, under the gitignored `fuzz/corpus/<target>/`. |
| **Corpus** | A collection of inputs. "Seed corpus" means the checked-in set; "working corpus" means the local/CI set. |
| **Coverage** | The code paths an input reaches. The fuzzer keeps an input when it reaches something new. |
| **Crash artifact** | The file libFuzzer writes when the target aborts, under `fuzz/artifacts/<target>/`. |
| **OOM** | Out of memory. A huge allocation can abort through the sanitizer even when the input itself is tiny. |
| **ASan** | AddressSanitizer, which aborts on invalid memory access and on allocation failure. |
| **libFuzzer** | The fuzzing engine. `cargo fuzz` is the Rust wrapper around it. |
| **`tmin`** | Minimise **one** failing input into a small reproducer. |
| **`cmin`** | Minimise a **corpus** into a coverage-equivalent subset for a seed refresh. |

## 2. Where a failure appears

There are two fuzzing lanes:

- **Fuzz smoke** (`.github/workflows/ci.yml`) runs on pushes and pull requests.
  It replays the checked-in seeds and fuzzes each target for 15 seconds. A
  failure here usually means a seed, the harness, or a recent change is broken.
- **Fuzz nightly** (`.github/workflows/fuzz-nightly.yml`) runs on a schedule
  and on demand. Each target gets a long budget in its own job.

The nightly job uploads two artifacts per target:

| Artifact | Contents | Retention |
| --- | --- | --- |
| `fuzz-artifacts-<target>-<run>-<attempt>` | Crash, timeout, and OOM reproducers | 30 days |
| `fuzz-corpus-<target>-<run>-<attempt>` | The minimised working corpus, or the unminimised one if the job failed before minimisation | 14 days |

Target names use underscores: `module_probing`, not `module-probing`. If
`gh` reports a 404, it is probably looking at the wrong remote; add
`--repo <owner>/<repo>`.

## 3. The playbook

### Step 1 — Read the failure and download the crash artifact

Identify the target and the failure class from the job log:

- `AddressSanitizer: ...` — memory error or OOM;
- `panicked at ...` — Rust panic;
- `out-of-time` / `timeout` — the target took too long;
- `deadly signal` / `SEGV` — invalid memory access.

Then download the **crash** artifact, not the corpus:

```bash
gh run list --workflow=fuzz-nightly.yml --limit 5
gh run download <run-id> --repo <owner>/<repo> \
  --pattern 'fuzz-artifacts-<target>-*' \
  -D /tmp/fuzz-nightly
```

The artifact extracts the files directly, for example
`/tmp/fuzz-nightly/fuzz-artifacts-module_probing-<run>-1/crash-<hash>`.

If you are unsure which artifact you need, download everything with
`gh run download <run-id> --repo <owner>/<repo> -D /tmp/fuzz-nightly`.

### Step 2 — Reproduce locally

Prerequisites: `cargo install cargo-fuzz --locked` and a nightly toolchain
(`rustup toolchain install nightly`).

```bash
cargo +nightly fuzz run <target> \
  /tmp/fuzz-nightly/fuzz-artifacts-<target>-*/crash-<hash> \
  -- -runs=1
```

This must fail with the same signature as CI: the same panic message, sanitizer
summary, or allocation size. Keep the original artifact; do not edit it.

If it does not reproduce:

- run it a few times — some failures are timing-dependent;
- check that you are on the same commit and target architecture as CI;
- check the stack trace rather than assuming the platform is at fault.

An environment-only failure is still worth recording, but do not add a seed for
an input that does not reproduce.

### Step 3 — Minimise the one input with `tmin`

`tmin` uses libFuzzer's crash minimiser to remove bytes that are not needed to
trigger the same failure.

```bash
mkdir -p /tmp/fuzz-case
cp <crash-artifact> /tmp/fuzz-case/original
cargo +nightly fuzz tmin <target> /tmp/fuzz-case/original
```

`tmin` leaves the original alone and writes a new minimised artifact under
`fuzz/artifacts/<target>/`; the command prints its path as
`Minimized artifact:`. Re-run step 2 against that file to confirm it still
fails in the same way.

For OOMs and timeouts, `tmin` may not converge. In that case trim the input by
hand (remove sections or bytes that are not needed) and verify each candidate
still triggers the same root cause. A regression seed of a few dozen bytes is
typical.

### Step 4 — Find the owner and the cause

Read the local stack trace. Decide which code owns the defect:

- **`selium`**: a workspace crate mis-handles the input.
- **`wasmtiny`**: the pinned engine dependency does. The workspace keeps a
  sibling checkout at `../wasmtiny`; make the minimal, spec-driven fix there.
  To build against it locally, uncomment the `path` entries in the root
  `Cargo.toml` as `AGENTS.md` describes; restore the git pin before merging.
- **The fuzz target itself**: the harness unwraps, asserts, or allocates
  without checking. Fix the harness rather than weakening the target.

Compare the expected behaviour with the owning repository's specification and
tests. For module loading, `wasmtiny`'s
`openspec/specs/wasm-module-loader/spec.md` is explicit that untrusted declared
minimums must be capped, not allocated.

Do not "fix" a failure by:

- raising `-rss_limit_mb` or `-timeout`;
- setting `ASAN_OPTIONS=allocator_may_return_null=1` and calling it solved;
- catching panics or returning success from the fuzz target;
- deleting the seed or excluding the input.

Those hide the evidence. A malformed input must produce an error, not an abort.

### Step 5 — Patch and add a regression test

Fix the cause in the owning repository and add a test that would have caught
it: the minimised input, an equivalent WAT fixture, or a focused unit test with
the oversized value. Run that repository's test suite.

If the fix is in `wasmtiny`, push it, then update every pin in `selium`:

- `Cargo.toml` (`[workspace.dependencies]` and `[patch.crates-io]`);
- `fuzz/Cargo.toml` (the fuzz workspace has its own dependency list);
- the root and `fuzz/` lockfiles.

Forgetting the fuzz workspace is a common way to leave CI and the nightly lane
on the old engine.

### Step 6 — Verify locally, then add the regression seed

First, replay the minimised input — this must now succeed:

```bash
cargo +nightly fuzz run <target> <minimised-artifact> -- -runs=1
```

Then add it to the checked-in seeds with a name that describes the bug:

```bash
cp <minimised-artifact> fuzz/seeds/<target>/table_min_overflow.wasm
FUZZ_TARGETS=<target> scripts/fuzz-smoke.sh
```

The smoke lane replays every seed, then fuzzes briefly. Run the full lane
(`scripts/fuzz-smoke.sh`) and the owning repository's tests before committing.
For a longer local check:

```bash
FUZZ_TARGETS=<target> FUZZ_SECONDS=300 scripts/fuzz-smoke.sh
```

A named seed is deliberate; do not paste the working corpus into `fuzz/seeds/`.

### Step 7 — Commit, push, and confirm CI

Commit the fix, the pin bump (if any), and the named seed together, or as two
reviewable commits. Push and confirm:

- the **Fuzz smoke** job passes on the pull request;
- the owning repository's tests and the `Gate` job pass.

To exercise the long lane without waiting for the schedule:

```bash
gh workflow run fuzz-nightly.yml --repo <owner>/<repo> -f seconds=300
gh run watch
```

A short manual run is a plumbing check, not proof of long-term coverage. If the
failure is platform-specific, make sure the CI architecture is the one that
passes; a local pass on a different architecture is not enough.

### Step 8 — Refresh the corpus only after green

The crash is fixed and the regression seed is committed. Now the rest of the
run's discoveries can be folded into the seeds:

```bash
# Download the corpus artifact from the green run (or the failed run).
gh run download <run-id> --repo <owner>/<repo> \
  --pattern 'fuzz-corpus-<target>-*' \
  -D /tmp/fuzz-nightly

mkdir -p fuzz/corpus/<target>
cp -R /tmp/fuzz-nightly/fuzz-corpus-<target>-*/. fuzz/corpus/<target>/

# Minimise the union of the existing seeds and the downloaded corpus.
cargo +nightly fuzz cmin <target> fuzz/seeds/<target> fuzz/corpus/<target>

git status --short fuzz/seeds
scripts/fuzz-smoke.sh
```

Review the seed delta (mostly hash-named files), run the smoke lane, and commit
it as a deliberate corpus refresh. The named regression seed from step 6 stays:
`cmin` minimises for coverage, and a crash reproducer may not add unique
coverage, so it can disappear from the minimised output.

Do not run `cmin` on `frame_codec`: it renames the hand-written seeds by
content hash and loses their descriptions. Add new `frame_codec` regression
seeds by hand.

## 4. Quick reference

| Task | Command |
| --- | --- |
| List nightly runs | `gh run list --workflow=fuzz-nightly.yml` |
| Download crash inputs | `gh run download <id> --pattern 'fuzz-artifacts-<target>-*' -D /tmp/fuzz-nightly` |
| Download corpus | `gh run download <id> --pattern 'fuzz-corpus-<target>-*' -D /tmp/fuzz-nightly` |
| Reproduce one input | `cargo +nightly fuzz run <target> <input> -- -runs=1` |
| Minimise one crash | `cargo +nightly fuzz tmin <target> <input>` |
| Minimise the corpus | `cargo +nightly fuzz cmin <target> fuzz/seeds/<target> fuzz/corpus/<target>` |
| Replay seeds + fuzz | `scripts/fuzz-smoke.sh` |
| Run one target longer | `FUZZ_TARGETS=<target> FUZZ_SECONDS=300 scripts/fuzz-smoke.sh` |
| Trigger a short nightly | `gh workflow run fuzz-nightly.yml -f seconds=300` |

## 5. Worked example: the module-probing table OOM

A nightly `module_probing` run produced a 43-byte input and:

```
AddressSanitizer: out of memory: allocator is trying to allocate 0x6fffffff0 bytes
```

The input is a wasm table section declaring four tables. The last legacy table
entry declares a minimum of `0x6FFFFFFF` elements. In `wasmtiny`:

- `crates/core/src/loader/parser.rs` (`parse_table`) caps table minima at
  `100_000` only in the `0x40` table-with-initialiser branch;
- the legacy branch stores the declared minimum unchecked;
- `crates/core/src/loader/validator.rs` (`validate_tables`) checks only
  `max < min`;
- `LoadedModule::from_module_parts` reaches `Table::new`, which allocates
  `vec![NullRef; min]`. `WasmValue` is 16 bytes, so `0x6FFFFFFF × 16` is
  `30,064,771,056` bytes (~28 GiB).

That contradicts the loader specification, which requires a huge table minimum
to be rejected rather than allocated. The fix belongs in `wasmtiny` (cap the
legacy branch and validate it), followed by a pin bump, a replay of the
minimised input, and a named seed such as
`fuzz/seeds/module_probing/table_min_overflow.wasm`.

The working order is therefore:

1. crash artifact → reproduce → `tmin`;
2. `wasmtiny` fix + regression test;
3. pin bump in both `Cargo.toml` files and both lockfiles;
4. replay passes → named seed → smoke lane;
5. push and confirm CI;
6. then download the corpus and `cmin` for a seed refresh.

## 6. Checklist

- [ ] Downloaded the `fuzz-artifacts-*` artifact, not the corpus.
- [ ] Reproduced the same failure locally.
- [ ] Minimised the input with `tmin`, keeping the original.
- [ ] Identified the owning repository and root cause.
- [ ] Fixed the cause (not the fuzzer limits) and added a regression test.
- [ ] Updated every dependency pin and both lockfiles if the engine changed.
- [ ] Replayed the minimised input successfully.
- [ ] Added it as a named seed and ran `scripts/fuzz-smoke.sh`.
- [ ] Pushed and confirmed the smoke and gate jobs.
- [ ] Only after green: downloaded the corpus and ran `cmin`.
