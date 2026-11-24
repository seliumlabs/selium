# Design: Harden Platform Foundations

## Context

See `proposal.md` for motivation. The implementation has a sound ABI/kernel/runtime/guest split and real WASM integration coverage, but the reviewed defects cross those boundaries. The ordinary format, Clippy, and non-ignored workspace suites pass; strict traceability currently reports 32 uncovered scenarios. The main specs also contain stale Tokio/network, CLI-routing, crate-layout, and system-guest claims, so implementation must use the running design and this change's deltas rather than perpetuate those stale requirements.

The change must preserve WASM-only guests, no WASI, fenced wasmtiny, hot attach, guest-owned policy, shared-memory data planes, and restart-based recovery. Consumer write isolation remains owned by `hardened-consumer-shared-memory`.

## Goals / Non-Goals

**Goals:**

- Make hostile-input handling memory-safe, bounded, and non-panicking.
- Make authority, wake, cancellation, quota, durability, and shutdown outcomes deterministic and testable.
- Keep compatibility wrappers while giving them one protocol implementation.
- Make verification and release claims correspond to executed evidence.
- Stage breaking API and ABI changes explicitly.

**Non-Goals:**

- No engine replacement, WASI, native guest deployment, consensus, multi-host work, or guest snapshotting.
- No wholesale runtime rewrite or new I/O noun.
- No duplicate implementation of consumer read-only shared-memory hardening.
- No claim of a broad host/guest compatibility window during the alpha; lockstep compatibility is acceptable initially.

## Decisions

### D1: Reproduce each defect before changing its implementation

Every confirmed finding receives a focused regression at the lowest layer that demonstrates it, plus a real WASM or external-path test where the host/guest boundary matters. Security tests include hostile, not merely valid, inputs. This prevents refactors from making defects harder to observe without proving they are gone.

Alternative considered: refactor first and add tests later. Rejected because the existing suite already passes over several defects.

### D2: Validate before allocating or forming typed pointers

Hostcall create reads a fixed compatibility preamble and checks the requested range against exported guest memory before allocating a payload buffer. A defined envelope ceiling applies before decode, with variant-specific limits after decode. `Instant::checked_add` replaces unchecked deadline addition.

`PointerBackend` gains explicitly aligned owned backing and effective-address alignment checks for every atomic operation, including wait/notify. Raw mappings retain their unsafe constructor contract, but safe methods still reject misaligned offsets. Multi-memory parsing derives maximum entry count from mapping size, uses checked arithmetic, and validates ranges before allocation or sub-mapping.

Alternative considered: rely on allocator behaviour and current layout constants. Rejected because safe public APIs cannot depend on unstated alignment accidents.

### D3: Authorisation is computed from the operation's complete target context

Certificate operations construct a scope from the requested tenant and evaluate `MintCertificate` against it. Delegation evaluates each grant independently with all of its selectors intersecting; there is no flattened root/tenant scope and no role-based exception. Cross-tenant regressions cover signing, revocation, spawning, and child grants.

Alternative considered: special-case bootstrap system guests. Rejected because bootstrap-only provision limits exposure but does not make scoped grants honest.

### D4: ABI compatibility precedes rkyv enum decoding

Each hostcall request starts with a small fixed-format magic/version preamble followed by the bounded rkyv envelope. The host validates magic and `SELIUM_ABI_VERSION` before interpreting variant indices. Missing or unsupported versions fail bootstrap/request creation loudly. During alpha, host and guest packages use lockstep ABI versions; a compatibility window requires separate evidence later.

Alternative considered: put the version inside `HostcallEnvelope`. Rejected because an incompatible enum layout may prevent reliable decoding of the version itself.

### D5: Resource lifetime uses ownership guards, not manual restoration

Immediately failed hostcall creates are polled for their structured error and dropped. Guest errors retain `AbiError` codes. Store/control session checkout and RPC establishment receive RAII guards whose `Drop` implementations restore, close, or reclaim resources. Join handles register the waker and re-check completion; timers re-check deadlines and propagate sleep failure.

Runtime quota reservations use guards that roll back on every error path and commit only after the resource operation succeeds. State preparation marks completion last. Runtime-owned providers are explicitly injected into host-side channel use; the process-global provider remains confined to a single WASM guest context. Runtime shutdown is idempotent and stops/joins owned background activity.

Alternative considered: add more manual cleanup branches. Rejected because cancellation and early-return paths are precisely what manual branches miss.

### D6: Volume append and durable acknowledgement have explicit commit boundaries

Volume registry state shares one append synchronisation domain per underlying volume across all handles, so offset lookup and append are ordered together. Store recovery runs before every first blob-store read or mutation, validates recovered content hashes, and treats existing content-addressed blobs as immutable. A torn or invalid recovered blob is not exposed.

Identity, accounting, store, and control-plane mutations follow durable append/sync, projection/enforcement, then acknowledgement. Failure before commit leaves the old visible state; failure after commit is reported and reconciled from the durable intent rather than hidden.

Alternative considered: best-effort logging followed by success. Rejected because restart then reverses operations callers were told succeeded.

### D7: One ring implementation and one stream state machine

`selium-shm::layout` owns reservation, frame validation, encoding, and decoding. `RingBuf` and channel wrappers delegate to it; public import paths remain through re-exports. Multi-memory attach uses the shared byte-channel geometry parser, preserving independent ring capacities.

`ByteStreamReader` tracks header/payload state across partial reads instead of assuming a 64 KiB frame ceiling. Writer shutdown releases registration and closes peer input. Framed-write state distinguishes finishing an older operation from accepting a new payload.

Alternative considered: patch individual wrapper checks. Rejected because parallel implementations have already drifted.

### D8: Live tables enforce single-writer authority before any arbiter

Initially, compare-and-set is available only where a table has an enforceable single-writer registration; competing construction or CAS fails loudly. A future arbiter may replace that authority without changing caller semantics, but independent local projections never constitute CAS authority. No consensus work is introduced.

Alternative considered: document local CAS as best effort while retaining its atomic-sounding API. Rejected because the result is observably incorrect under competing writers.

### D9: Discovery and connector convergence is event-driven and transactional

Registration replacement updates primary, label, alias, and back-reference state coherently. HTTP and QUIC connectors consume route-watch replacement/revocation events and retain attachment failure as a fallback eviction. Identity periodically republishes durable domains so a restarted discovery converges.

The HTTP codec returns distinct incomplete, malformed, and oversized outcomes. Invalid or conflicting lengths and incomplete trailer termination are rejected at the request boundary.

### D10: Traceability combines mechanical adjacency with reviewed semantic evidence

The checker associates annotations only with the immediately following test function and rejects detached declaration blocks. Scenario review confirms that the attached assertions exercise the named behaviour; no script is claimed to prove intent. The 32 currently uncovered scenarios receive real tests or specific reviewed waivers, never blanket waivers.

Real HTTPS and CLI subprocess spines are added. Format-level torn-write tests retain that accurate name; new fault-injection tests interrupt actual store, identity, and control-plane persistence boundaries. Checked-in stable/nightly versions and utility versions replace moving tags, and all artifact guards honour `CARGO_TARGET_DIR`.

### D11: Release is consumer-driven and initially lockstep

Before publication, internal dependencies carry versions and required patches become released upstream fixes or owned published fork packages. Release automation runs ordered dry-runs and an isolated consumer guest build/instantiation. Platform artifacts include `sel`, the runtime, and matching system-guest WASM, with smoke tests on every advertised target. The crates.io job stays disabled until those checks exist.

Alternative considered: publish host and guest crates independently now. Rejected because repository patches are not inherited downstream and mixed rkyv versions are unsafe.

### D12: A change's own deltas are required evidence before it archives

Traceability covers the shipped specs *and* the deltas of every unparked active
change. Otherwise a change can report a green gate while its new scenarios have
no adjacent test, and the gap only appears after the deltas are synced at
archive. Parked changes (explicitly outside the release gate) are excluded, so
future proposals do not force speculative coverage.

Alternative considered: annotate at archive time. Rejected because the change's
completion claim would then rest on a state the gate has not checked.

### D13: Durability has an explicit barrier, not an implied one

The store batches appends for throughput, so "the append returned" is not the
same as "the record survives a power cycle". Policy writers that acknowledge to
a caller (identity, accountant, control plane) ask for the barrier explicitly
(`StoreRequest::Sync`) after the append and before projecting or
acknowledging; high-volume bookkeeping keeps the store's batching. The store
still owns durability policy — the barrier is one of the times it syncs, not
the only one.

Alternative considered: make every append synchronous. Rejected because the
accountant's per-second usage records do not need a per-record fsync, and the
store's threshold policy exists for exactly that.

### D14: Refactor only behind the new regression net

After correctness fixes, split hostcall dispatch by resource family and reduce shared-state transaction surfaces incrementally. Preserve public behaviour and avoid a monolithic rewrite. Reconcile README, known-unknowns, DESIGN-INTENT current-state text, CLI/runtime main specs, and stale active changes in the same change so docs again describe what runs.

## Risks / Trade-offs

- **Aligned backing changes mapping construction costs** → centralise allocation and benchmark ring paths; correctness takes precedence over undocumented allocator luck.
- **RAII cancellation can close a session rather than reuse it** → prefer safe closure when restoration is not provably valid; test subsequent requests and region counts.
- **Single-writer live tables reject previously accepted constructions** → fail at construction/CAS with a clear migration error and update in-tree publishers to one authority.
- **ABI preambles break all older guests** → rebuild in lockstep, emit explicit mismatch errors, and keep this as the alpha migration rather than pretending mixed versions work.
- **Offline-root migration may leave a legacy key file after I/O failure** → fail boot loudly and document manual remediation; never report successful removal prematurely.
- **Durable-before-visible ordering increases latency and exposes partial application states** → make commit boundaries explicit (append + `Sync` for policy writes) and test recovery at each boundary.
- **Traceability adjacency requires reorganising tests** → move annotations with their evidence and add missing behavioural tests instead of weakening the checker.
- **Release smoke matrices increase CI time** → run expensive target artifact lanes on tags while keeping the Linux correctness gate required for merges.

## Migration Plan

1. Land regression tests and P0 input-safety/authority fixes; keep all existing golden paths green.
2. Add operation/cancellation/quota/runtime lifecycle guards and volume/store durability fixes.
3. Consolidate protocol paths and introduce the single-writer table constraint with in-tree migrations.
4. Introduce the ABI preamble and structured guest errors/timer results as one lockstep guest rebuild; provide clear compiler/runtime migration errors.
5. Migrate persistent keyrings: validate existing hierarchy, remove legacy online root private keys, and verify restart without them.
6. Restore semantic traceability and add real external/component-crash coverage; update known-unknowns and current-state docs.
7. Reconcile active/main specs, pin tools, then enable consumer-tested release automation only after all prior gates pass.

Rollback is per stage before the ABI preamble. After the preamble lands, rollback requires rolling host and guest artifacts back together. Durable data remains compatible; the root-key migration is reversible only from an operator-held offline backup, so validation occurs before removal and failure stops boot.
