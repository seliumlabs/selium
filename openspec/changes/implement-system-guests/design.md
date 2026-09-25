## Context

arch3 requires five system guests to make the platform self-managing. The host/guest foundation from `specify-host-guest-foundation` has been implemented and archived, so this change should target the current crate layout and API surface instead of the older proposal model.

Important current facts:

- core foundation crates are `crates/core/abi`, `crates/core/kernel`, `crates/core/runtime`, `crates/core/guest`, and `crates/core/guest/macros`
- guest-side I/O patterns are in `crates/libs/io` as `selium-io`, not in `selium-guest`
- `selium-runtime` bootstraps system guests from `SystemGuestDescriptor` values with module bytes, entrypoint, arguments, grants, dependencies, and readiness conditions
- `selium-guest-macros::entrypoint` currently supports async zero-argument entrypoints returning `()`
- `selium-guest-macros::pattern_interface` emits metadata only; it does not provide a transport or dispatcher
- `selium-kernel` network resources are protocol-neutral and do not by themselves imply QUIC or mTLS

The system guests still serve the roles described in `ARCHITECTURE.md`, but their interactions need to be framed around durable logs, shared-memory topics, live tables, activity streams, network request exchanges, and explicit runtime bootstrap descriptors.

**Current state:**
- The foundation is implemented, archived under `openspec/changes/archive/2026-04-30-specify-host-guest-foundation/`, and reflected in `openspec/specs/`
- No system guests are implemented in arch3 yet
- System guests will live under `crates/guests/...`
- `SUMMARY.md` records the current split: `selium-guest` wraps primitive handles and `selium-io` owns guest-side I/O patterns

**Constraints:**
- Each system guest runs as `wasm32-unknown-unknown`
- `selium-runtime` bootstraps system guests generically from host configuration
- System guests should remain host-local for day 1 even if the state they coordinate becomes cluster-visible
- Single-host operation comes first; cross-host coordination is introduced incrementally
- Initial guest entrypoints should be zero-argument `#[entrypoint]` functions unless macro support for arguments is added in this change
- QUIC and mTLS must be treated as runtime/network bridge concerns until concrete guest-facing support exists

## Goals / Non-Goals

**Goals:**
- Define how the five system guests build on the new foundation crates
- Keep the host runtime generic and move policy into guests
- Frame guest interaction in terms of `selium-io` topics/live tables, durable logs, network request exchanges, and shared state rather than RPC privilege
- Preserve the state-machine model for scheduler and related guests
- Produce a clean sequencing plan: scaffolding, cluster/discovery, scheduler/supervisor, external-api, integration

**Non-Goals:**
- Re-specifying or replacing the foundation crates
- Moving `selium-io` patterns back into `selium-guest`
- Implementing queue-first transport semantics as the centre of guest communication
- Solving large-cluster topology changes, replication, or migration here
- Defining production transport security beyond the current runtime/network bridge unless a concrete missing API is discovered during implementation

## Decisions

### 1. System guests depend on the implemented foundation crates

**Decision:** `implement-system-guests` uses the current `selium-abi`, `selium-kernel`, `selium-runtime`, `selium-guest`, `selium-guest-macros`, and `selium-io` crates as its foundation.

**Rationale:** These crates now exist and provide the stable substrate for capabilities, selectors, handles, hostcalls, bootstrap, entrypoint metadata, and guest-side I/O patterns. The system guest proposal should not duplicate or contradict them.

**Alternative considered:** Implement guests and foundation together in one pass.
- Rejected because it would blur architectural boundaries and make the system guest specs unstable.

### 2. Guest I/O patterns come from `selium-io`

**Decision:** System guests use `selium-io` for shared-memory-backed channels, pub/sub topics, and live tables. `selium-guest` is used for primitive handles, typed codecs, logging, platform calls, and macro integration.

**Rationale:** The current codebase moved the pattern library into `selium-io`. Keeping this boundary explicit prevents implementation from depending on APIs that no longer exist in `selium-guest`.

**Alternative considered:** Treat `selium-guest` as the sole pattern layer.
- Rejected because it contradicts the current crate split and would create duplicate abstractions.

### 3. Scheduler remains state-machine-centric

**Decision:** Scheduler state is coordinated through scheduler-owned durable/logged state and live-table projections, then reconciled against observed host process state.

**Rationale:** Placement is fundamentally shared-state coordination. A request exchange can initiate intent, but the durable truth must live in scheduler-managed state that can be replayed and observed.

**Alternative considered:** Make scheduler primarily a command processor.
- Rejected because it weakens replay, visibility, and convergence semantics.

### 4. Supervisor reacts to runtime activity and state changes

**Decision:** Supervisor consumes runtime lifecycle and metering signals, maintains health and restart policy state, and emits restart or recovery intent through the guest pattern layer.

**Rationale:** Supervisor is reactive by nature. It should subscribe to events and state changes, not poll or rely on bespoke imperative hooks.

**Alternative considered:** Give supervisor direct host control hooks for bespoke recovery flows.
- Rejected because it would smuggle policy back into the host layer.

### 5. Discovery owns URI and interface visibility

**Decision:** Discovery maintains URI mappings and guest-visible interface metadata, serving both exact lookup and prefix-oriented discovery.

**Rationale:** This keeps URI ownership centralised and lets other guests resolve resources and interfaces without embedding topology assumptions.

**Alternative considered:** Let each guest publish and resolve its own interfaces independently.
- Rejected because it fragments naming and discovery policy.

### 6. Cluster coordinates host-visible shared state, not arbitrary guest logic

**Decision:** Cluster tracks host membership, host load, and cross-host shared-state bootstrap. Day 1 may run single-host while exposing the same state surfaces scheduler and other guests will consume.

**Rationale:** Cluster should not become a dumping ground for all distributed concerns. Its job is to expose cluster facts and shared-state coordination inputs.

**Alternative considered:** Put routing, placement, and recovery policy inside cluster.
- Rejected because it would centralise too much policy in one guest.

### 7. External API is a narrow intent interpreter

**Decision:** `selium-external-api` accepts user intent from an externally authenticated network path, resolves the relevant guest interfaces, uses the appropriate state/topic/request-exchange interaction, and returns useful feedback.

**Rationale:** External API should orchestrate by decomposition and delegation, not by reimplementing scheduler or supervisor policy. Current protocol-neutral network primitives do not justify hard-coding guest-owned QUIC or mTLS semantics in this proposal.

**Alternative considered:** Let external-api write directly into every guest's private state structures.
- Rejected because it breaks guest ownership boundaries.

### 8. Bootstrap order is declarative and host-generic

**Decision:** The host runtime uses configuration to express guest descriptors, dependencies, and readiness checks. The system guest change defines the required dependencies, but not guest-specific runtime code.

**Rationale:** The host should know how to bootstrap generically, not how to bootstrap scheduler specifically.

**Alternative considered:** Hard-code a startup order in host implementation.
- Rejected because it violates the smart-guest / dumb-host direction.

### 9. Initial guest entrypoints are zero-argument

**Decision:** Initial system guest entrypoints use zero-argument `#[entrypoint]` functions and acquire configuration through handles, durable configuration resources, or later explicit macro/runtime support.

**Rationale:** The runtime descriptor supports arguments, but the current macro rejects entrypoint arguments. Aligning with the implemented macro avoids starting actioning with generated-code failures.

**Alternative considered:** Specify argument-bearing macro entrypoints now.
- Rejected unless this change first extends and tests `selium-guest-macros`.

## Risks / Trade-offs

- **[Guest boundaries may still drift toward RPC-only design]** -> Keep guest specs explicit about which interactions are request/reply and which are state, subscription, or stream based.
- **[Cross-host state remains hard]** -> Keep day 1 behaviour single-host capable and introduce cross-host coordination as an incremental extension.
- **[Bootstrap dependencies may become circular]** -> Model dependencies and readiness declaratively through runtime config and guest-owned readiness signals.
- **[Shared-memory-first design may still need flow control]** -> Depend on `selium-io` channel semantics and the foundation's signalling primitives rather than reintroducing queue assumptions at the guest level.
- **[System guests need broad authority]** -> Rely on scoped capability selectors from the foundation change and make each guest's authority explicit.
- **[Protocol security assumptions may outrun code]** -> Keep QUIC/mTLS requirements at the runtime/network bridge boundary until guest-facing support is implemented.

## Migration Plan

1. Create `crates/guests/cluster`, `crates/guests/discovery`, `crates/guests/scheduler`, `crates/guests/supervisor`, and `crates/guests/external-api` as workspace crates.
2. Add zero-argument guest entrypoints, interface metadata, logging, and native-test seams for each crate.
3. Define the runtime `SystemGuestDescriptor` set, scoped grants, dependencies, and readiness conditions.
4. Implement guests in dependency order: cluster, discovery, scheduler, supervisor, external-api.
5. Validate single-host control-plane behaviour end to end.
6. Add limited cross-host coordination once the single-host behaviour is stable.

**Rollback:** Because this remains pre-implementation design work, rollback means reverting these OpenSpec artifacts and deferring the system guest change until the foundation stabilises further.

## Open Questions

1. Which guest interfaces need day 1 request-exchange APIs, and which should remain state, topic, or live-table only?
2. What exact readiness signals should each system guest expose for runtime bootstrap?
3. How much interface metadata should discovery own versus read from macro-generated guest metadata?
4. Which runtime/network bridge work is required before external-api can truthfully claim QUIC and mTLS support?
5. Which cluster coordination behaviours are day 1 requirements versus future work tied to the cluster-scaling and channel-replication changes?
