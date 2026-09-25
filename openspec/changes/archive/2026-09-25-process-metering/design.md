# Design

## Context

See proposal.md — Why. The runtime already runs a one-second metering ticker, a `MeteringProjector` (cpu/bandwidth/storage accumulators, memory derived from shared-region ownership), and a kernel metering hook the accountant reads via `MeteringRead`. CPU is a placeholder (`record_cpu_usage` is public but nothing in production calls it); the accountant computes `hard.cpu_micros` in `enforcement_for` and discards it. Wasmtiny removes the engine metering Selium depends on, so this design specifies the seam across the two repositories.

## Goals / Non-Goals

**Goals:**

- Make CPU a billable, engine-honest quantity: per-process, monotonic, instruction-based.
- Make memory a billable gauge: committed linear-memory pages, peaked per window.
- Author a per-tenant per-minute CPU ceiling from the accountant's existing plan+overage, and halt a process that exhausts it.
- Keep the meter honest and load-invariant; keep pricing outside the meter.

**Non-Goals:**

- Resumable preemption (quantum/epoch scheduling) and migration — explicitly out of scope; the three seam rules below keep them open later.
- AOT-path instruction instrumentation (a Wasmtiny follow-up).
- Billing shared-region bytes as "memory" (regions stay a separate quota dimension).
- Changing the one-second sampling cadence or the per-minute window structure already in place.

## Decisions

### D1 — Instructions are the CPU unit of account; `cpu_micros` removed

Executed instructions are deterministic, monotonic, tamper-evident, and invariant under host load — the only CPU quantity that survives the multi-threaded, wake-inlined reactor while remaining fairly attributable. Wall-clock microseconds leak neighbour contention and cannot be enforced honestly.

**Alternatives considered:** (a) keep `cpu_micros` and convert with a nominal constant — rejected, it is a fake measurement upstream of a real one; (b) real per-thread CPU via `getrusage` — rejected, inline cross-guest wakes attribute guest B's work to guest A (see the wake path's "enqueue + inline poll").

### D2 — Memory gauge is committed linear-memory pages

The engine reports owned-pages-per-instance; the runtime sums them per process. Shared-region bytes leave the memory gauge (they are channel capacity, already quota-enforced via `ResourceClass::SharedRegion`); durable bytes stay in `storage_bytes`. Memory bills at peak per window (already how `merge_gauges` behaves — now deliberate).

### D3 — Per-minute windows, counters differenced, gauges peaked

The accountant's existing per-minute window and one-second sampling stay. `cpu_instructions` joins `bandwidth_bytes` as a differenced counter; `memory_bytes` stays a peaked gauge.

### D4 — Enforcement is "halt + reap", with a timing seam

When a process's current-window instruction count reaches its tenant's ceiling, the Wasmtiny budget check stops execution (a distinct budget-exhausted outcome) and the runtime reaps through the existing teardown path — the same path entrypoint completion and traps already use, so region/pipe/process-slot release comes for free. Resumable preemption (suspend-and-resume) is deliberately not built.

The budget is anchored **at spawn** and re-anchored on each **wall-clock minute boundary** (the same UTC-minute alignment the accountant's billing windows use), and re-applied on every one-second sampling tick. Anchoring at spawn closes the gap where a process spawned mid-window would otherwise run its whole first window unbounded; the per-tick re-application bounds a ceiling change's propagation latency to one sampling interval.

### D5 — Three seam rules that keep preemption open later

1. **Exhaustion is a first-class outcome**, not an inline teardown: the engine surfaces a distinct budget-exhausted signal; the runtime decides what it means (trap/reap today, suspend/resubmit later).
2. **Budgets are settable and resettable**, never welded into instance construction: per-minute ceiling republication and a future throttle-to-zero both need this.
3. **The consequence lives in the host, not the engine**: the engine reports; the runtime chooses reap vs. resubmit vs. wait — "dumb host, smart guest" applies to the metering boundary too.

### D6 — Ceiling delivery and unit conversion at the pricing seam

The accountant authors a per-tenant per-minute instruction ceiling from plan+overage and delivers it as an authored quota under a dedicated `ResourceClass::Cpu` (a quota dimension, not an allocatable resource). Conversion from operator-facing units to instructions happens exactly once, at the rate-card boundary, so the metering pipeline never carries a pseudo-time quantity. Enforcement is in the engine per process, not a kernel allocation chokepoint — CPU has no single "allocation" event to gate.

### D7 — Two-repository split

Engine responsibility (counter, page gauge, budget check, distinct trap) lives in the Wasmtiny `instance-metering` change; contract responsibility (observation type, projection, reduction, authoring, reap wiring) lives here. The Wasmtiny proposal is the dependency; this proposal is the consumer.

## Risks / Trade-offs

- **[ABI break]** Removing `cpu_micros` ripples through `selium-service` FlatBuffers tables, the guest read API, and tests → regenerate FlatBuffers with instruction fields; update every test asserting `cpu_micros`; keep the removal in one change so the break is auditable.
- **[Region bytes leave the memory gauge]** Region consumption stops appearing in the billing ledger's memory column → it remains quota-enforced and visible through quota counters; revisit only if operators need region bytes billed.
- **[Ceiling propagation latency]** A process can overshoot within a one-second cadence before a new ceiling lands → overshoot is bounded by one sampling interval; acceptable because the meter is honest about the overshoot.
- **[Instruction-count unit unfamiliar to operators]** "instructions/minute" is a poor plan number → the rate card owns the human-units conversion; meter stays pure.
- **[Engine overhead]** Per-instruction charging has a cost → handled on the Wasmtiny side by batching/amortising the counter flush (see that change's design).

## Migration Plan

1. Land Wasmtiny `instance-metering` (additive; no breaking change there).
2. Land this change: swap `MeteringObservation` to `cpu_instructions`, regenerate FlatBuffers, wire the projector to read engine stats, add budget enforcement, and update the accountant to author CPU ceilings.
3. Update all tests and golden-path assertions from `cpu_micros` to `cpu_instructions`.

Rollback is a revert of this change plus the Wasmtiny caller alongside; there is no data migration—metering state is ephemeral per process.

## Open Questions

- Whether `.aot` native execution needs instruction instrumentation once it is on Selium's execution path (Wasmtiny open question; interpreter covers today's path).
- Whether operators eventually want shared-region bytes reported alongside linear pages (a billing-product question, not a metering-correctness question).
