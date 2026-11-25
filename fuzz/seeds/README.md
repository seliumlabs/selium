# Fuzz seed corpus

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
seeds (`bad_checksum`, `truncated`, `valid_empty_ready`, ...). Keep those names
when editing them: the seed is a test case and the name is the description.

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

Do not copy the artifact straight into `fuzz/seeds/`: minimise it first, as
below, so the committed corpus stays small and reviewable.

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

Copy a crash input from `fuzz/artifacts/<target>/` into
`fuzz/seeds/<target>/` with a descriptive name, and reference it in the commit
message. Prefer a small, hand-trimmed input over the raw artifact when the
failure reproduces with less data.
