## Context

See proposal.md - Why. The substrate this change builds on:

- `MeteringObservation { cpu_micros, memory_bytes, storage_bytes, bandwidth_bytes }` already exists per process; the kernel stores the latest observation and the runtime projects it via `project_metering`. Guests read via `MeteringRead` (pull) or the activity log's `MeteringObserved` events (message-string only, no structured fields).
- `ProcessTenant` hostcall returns a process's tenant; lifecycle events (`ProcessStarted`/`ProcessExited`) carry `process_id`, giving the bookkeeper an inventory without a new enumeration primitive.
- `SystemGuestDescriptor` bootstraps a module by name + entrypoint name; `#[entrypoint]` emits a distinct export per function, so one module can carry two entrypoints.
- `DelegateGrants`/`MintCertificate` set the precedent for bootstrap-only, non-conferable capabilities; the bridge-server already confers identity-resolved grants via `DelegateGrants`.
- Cross-host guest channels do not exist yet (`channel-replication`, `cluster-scaling` are separate pending changes); v1 stays single-host.

## Goals / Non-Goals

**Goals:**

- A cool policy loop: per-second reduction to buckets, per-minute billing, rare account-state transitions; enforcement stays preemptive at the chokepoints.
- Quota as a distinct host primitive, not a grant; the accountant is its sole author.
- Durable usage ledger with restart-based recovery.

**Non-Goals:**

- Cross-host bucket fan-in (designed, deferred to the platform's cross-host routing).
- Byte-accurate billing ledger resilience across host failure (counter checkpoints are a later concern).
- Connector rate-band consumption (the accountant authors bands; the connector consumes them later).
- Billing prices/invoicing formats — the ledger stores usage; money is downstream.

## Decisions

### 1. Bookkeeper vs accountant: one crate, two entrypoints

**Decision:** `selium-accountant` ships two `#[entrypoint]` functions — `bookkeeper` (per host) and `accountant` (single logical instance) — bootstrapped as two descriptors of the same module.

**Rationale:** The macro emits per-function exports and the runtime selects by entrypoint name, so roles are mechanically clean; shared bucket/ledger types compile once.

**Alternative considered:** Runtime-as-client pushing feeds. Rejected — makes the host a workload peer on its own fabric, inverting dumb-host/smart-guest.

### 2. Poll locally, reduce locally, publish buckets

**Decision:** The bookkeeper maintains a process inventory from lifecycle events, polls `MeteringRead` per local process each second, differences cumulative counters (cpu/bandwidth) and samples gauges (memory/storage), sums into per-tenant buckets, and publishes buckets to a shared-memory topic.

**Rationale:** Reduction is the scaling lever — the fabric traffic is `O(tenants)`, not `O(processes)` — and cumulative counters make the feed loss-tolerant: a dropped second is recovered by the window's counter delta.

**Alternative considered:** A structured push feed from the host into the accountant. Rejected — sub-minute latency the cool loop does not need, and it adds a high-frequency write through the activity log.

### 3. Quota as a host primitive, authored by the accountant

**Decision:** `QuotaSet`/`QuotaClear` hostcalls gated by a bootstrap-provisioned, non-conferable `QuotaWrite` capability. The host consults the quota table synchronously at allocation; grants and quotas stay separate. Dimensions: shared-memory bytes at region allocation (released at free/teardown), storage bytes at log append and blob put (sticky — durable bytes persist; user-side storage management, e.g. a CLI capability, is TBA), and queued pipe items at enqueue (released at receive/teardown). Usage is tracked even before a ceiling is authored so an accountant `QuotaSet` inherits live usage. Denials fail with the distinct `AbiErrorCode::QuotaExceeded` naming tenant and dimension. A queue handoff transfers resource ownership and its quota reservation to the receiver (force-accepted — a poisoned handoff can push the receiver over its ceiling; a known open vector metering surfaces).

**Rationale:** Grants admit a class of operation; quotas cap its extent. Folding quotas into grants would fake enforcement; a guest round-trip per allocation would destroy throughput.

**Alternative considered:** Quotas embedded in `CapabilityGrant` selectors. Rejected — the grant system has no counting semantics.

### 4. Single-host v1, bucket topic is shared memory

**Decision:** In v1 the bookkeeper and accountant co-locate on one host and exchange buckets over a shared-memory topic. Cross-host fan-in is a later consumer of the platform's cross-host routing.

**Rationale:** No transport problem exists single-host, so the loop is fully buildable today; the guest's design is unchanged when the topic becomes reachable cross-host.

### 5. Cool loop cadence

**Decision:** Sample at 1 second, bill at 1 minute, transition account state on rare events (operator/billing writes, delinquency). The accountant never sits on the per-event enforcement path.

**Rationale:** Enforcement is preemptive (quota at allocation, token bucket at the edge, narrowing at conferral); the loop only adjusts knobs.

### 6. Ceilings and account state machine

**Decision:** Soft ceiling = plan; hard ceiling = plan + opt-in overage. States: paid, in-overage, at-budget, delinquent. Delinquency narrows grants to nothing and zeroes quotas; restoration restores knobs.

**Rationale:** This encodes the opt-in overage-budget revenue model as a small finite state machine, which the dumb host expresses as enforcement values rather than policy.

### 7. Durable ledger

**Decision:** The accountant appends per-minute per-tenant usage to a durable log (replayed on restart). Retention and rollup policy are operator-set.

**Rationale:** Host metering is a rolling snapshot; billing needs append-only history that survives guest restart.

### 8. Metering producer

**Decision:** The runtime gains a per-second ticker (started at bootstrap) projecting cumulative cpu/bandwidth counters and current memory/storage gauges into the kernel. Bandwidth is instrumented on the TCP send/recv paths; per-process CPU accounting is an instrumentation hook until the WASM-resources change lands.

**Rationale:** The accountant cannot aggregate data nobody produces; cumulative semantics make the best-effort bucket feed self-healing on loss.

## Risks / Trade-offs

- **[Metering producer is unfinished host work]** → Make the runtime ticker an early, isolated task; the accountant spec targets a placeholder producer until it lands.
- **[Single accountant is a SPOF]** → Durable ledger + restart-based recovery; tenant sharding is a later scaling escape, not a v1 constraint.
- **[Quota denial correctness]** → Enforce in the same dispatch sites as existing capability checks, with tests naming tenant and dimension.
- **[Ledger growth]** → Operator-set retention and per-minute rollup keep it bounded; compaction is a later task.
- **[Billing couples to single-host scope]** → Accept in v1; cross-host fan-in restores cluster-wide billing when routing lands.

## Migration Plan

1. Add `QuotaWrite` and quota hostcalls to `selium-abi`.
2. Add the host-held quota table and admission to the runtime and kernel.
3. Add the per-second metering projector to the runtime.
4. Add the `crates/guests/accountant` crate with the `bookkeeper` entrypoint (inventory, sampling, reduction, bucket topic).
5. Add the `accountant` entrypoint (merge, windows, ledger, ceilings, state machine, `QuotaSet`/narrowing writes).
6. Fold narrowing into the bridge-server's conferral.
7. Validate the single-host loop end to end: workload runs → buckets flow → ledger records → quota denies over-ceiling allocation.

**Rollback:** Pre-implementation design work; rollback reverts these OpenSpec artifacts and defers the change.

## Open Questions

- Ledger retention and compaction cadence — operator policy, does not change specs.
- Overage pricing/invoice representation — downstream of the ledger, deferred.
- Cross-host bucket transport primitive — deferred to cross-host routing work.
- CPU-hard-enforcement depth — depends on "WASM resources" work; memory/storage/pipes quotas are enforceable now.
