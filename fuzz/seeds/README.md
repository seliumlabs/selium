# Fuzz seed corpus

New to fuzzing? Read [`docs/fuzzing-playbook.md`](../../docs/fuzzing-playbook.md)
first: it explains triage, `tmin` versus `cmin`, and the whole failure workflow.
This file is the reference for the corpus itself.

Each `fuzz/seeds/<target>/` directory is the checked-in input corpus for that
target. `scripts/fuzz-smoke.sh` replays it before fuzzing, so a seed that makes
a target crash, hang, or exhaust memory fails CI in seconds.

Inputs discovered by a run are written to the gitignored
`fuzz/corpus/<target>/` instead, so a local or CI fuzz run never dirties the
tree. The scheduled nightly workflow (`.github/workflows/fuzz-nightly.yml`)
extends the budget per target, keeps that working corpus in the Actions cache
for its next run, and uploads both crash reproducers and the minimised corpus
as workflow artifacts.

## Naming

Most files are named by the SHA-1 of their contents, which is libFuzzer's
convention. `frame_codec` keeps human-readable names for its hand-written
seeds (`bad_checksum`, `truncated`, `valid_empty_ready`, ...), and new
regression seeds should also get a descriptive name
(`table_min_overflow.wasm`). Keep those names when editing them: the seed is a
test case and the name is the description.

## `cmin` or `tmin`?

They minimise different things:

| Command | Minimises | Use it for |
| --- | --- | --- |
| `cargo fuzz tmin <target> <one-input>` | One crashing input into the smallest input that still triggers it | Triage and regression seeds |
| `cargo fuzz cmin <target> <out-dir> <corpus-dir>` | A whole corpus into a coverage-equivalent subset | Refreshing `fuzz/seeds/` |

Using `cmin` on a single crash does not produce a minimal reproducer; using
`tmin` on a corpus is not a corpus refresh.

## Pulling a nightly corpus from CI

The Actions cache is only visible to workflows and cannot be downloaded from a
developer machine. Use the `fuzz-corpus-<target>-*` artifact instead (retained
for 14 days; crash artifacts for 30):

```bash
# Choose a scheduled or workflow_dispatch run.
gh run list --workflow=fuzz-nightly.yml --limit 5

# Download just one target's corpus. The artifact contents are the files
# themselves, not a fuzz/corpus/... tree.
gh run download <run-id> --pattern 'fuzz-corpus-<target>-*' -D /tmp/fuzz-nightly

mkdir -p fuzz/corpus/<target>
cp -R /tmp/fuzz-nightly/fuzz-corpus-<target>-*/. fuzz/corpus/<target>/
```

Add `--repo <owner>/<repo>` if `gh` reports a 404 because it resolved the wrong
remote, and use underscores in the target name (`module_probing`, not
`module-probing`).

This is the **corpus**, not a crash. If a run failed, download
`fuzz-artifacts-<target>-*` and follow
[`docs/fuzzing-playbook.md`](../../docs/fuzzing-playbook.md) before refreshing
anything. Do not copy an artifact straight into `fuzz/seeds/`: minimise it
first, as below, so the committed corpus stays small and reviewable.

## Refreshing a minimised corpus

The committed corpus is a minimised snapshot, not a dump of every input a run
ever found. After a longer run has populated `fuzz/corpus/<target>/`, refresh
a target's seeds with `cargo fuzz cmin`:

```bash
cargo +nightly fuzz cmin <target> fuzz/seeds/<target> fuzz/corpus/<target>
```

The first directory is the output (and receives the minimised union); the
remaining directories are merged into it. Re-run the smoke lane afterwards and
commit the result as a deliberate corpus refresh.

Do not run `cmin` on `frame_codec`: it renames the hand-written seeds to content
hashes and loses their descriptions.

## Adding a regression seed

After fixing a crash, minimise that one input with `tmin` (not `cmin`) while
keeping the original artifact:

```bash
mkdir -p /tmp/fuzz-case
# The crash is either a downloaded CI artifact or a local fuzz/artifacts/ file.
cp <crash-artifact> /tmp/fuzz-case/original
cargo +nightly fuzz tmin <target> /tmp/fuzz-case/original
```

`tmin` leaves the original alone, writes a new minimised artifact under
`fuzz/artifacts/<target>/`, and prints its path. Replay the minimised file with
`cargo +nightly fuzz run <target> <minimised> -- -runs=1`; after the fix it
must exit cleanly. Copy it into `fuzz/seeds/<target>/` with a descriptive name,
reference the original failure in the commit message, and run
`scripts/fuzz-smoke.sh`.

Add the seed only after the fix. A seed that still crashes turns the merge-gate
smoke lane red.

The full sequence, including dependency pin bumps and the corpus refresh after
CI is green, is in
[`docs/fuzzing-playbook.md`](../../docs/fuzzing-playbook.md).
