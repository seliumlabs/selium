## Purpose

Process-level metering of guest CPU (executed instructions) and memory (committed linear pages), reduced through per-minute billing windows into per-tenant usage and enforced by halting a process that exhausts its budget.

## Requirements

### Requirement: Instruction Unit of Account

The system SHALL measure a process's CPU consumption as a cumulative count of executed WebAssembly instructions. `MeteringObservation` SHALL carry the count as `cpu_instructions`, and SHALL carry no other CPU field.

#### Scenario: Observation carries instructions

- **WHEN** a metering observation is projected for a process
- **THEN** it SHALL carry the process's cumulative executed-instruction count in `cpu_instructions`
- **AND** SHALL NOT carry a `cpu_micros` field

### Requirement: Monotonic Per-Process Counters

A process's executed-instruction counter SHALL be monotonic over the process's lifetime and SHALL count only the guest WebAssembly instructions executed for that process, regardless of which host thread executed them.

#### Scenario: Counter never decreases

- **WHEN** a process's instruction counter is sampled at successive ticks
- **THEN** the later sample SHALL be greater than or equal to the earlier one

#### Scenario: Cross-thread execution attributed to the process

- **WHEN** a process's reactor is driven by different host threads across polls
- **THEN** all instructions the process executes SHALL be charged against that process

### Requirement: Aggregation Across Workers

A process's metered CPU and memory SHALL aggregate consumption across every host thread that executes it — including a multithreaded guest's dedicated worker pool — into a single per-process observation; the number of workers SHALL NOT change how consumption is attributed or how many observations are reported.

#### Scenario: Worker pool charges the process once

- **WHEN** a multithreaded guest consumes CPU on several worker threads during one metering interval
- **THEN** the process's reported CPU consumption SHALL reflect the sum across its workers as one per-process observation

### Requirement: Committed Linear Memory Gauge

The system SHALL gauge a process's memory as its committed linear-memory pages (the owned pages of its WebAssembly memories), distinct from shared-region bytes and durable storage bytes.

#### Scenario: Heap growth reflected in the gauge

- **WHEN** a guest grows its linear memory
- **THEN** the process's next memory gauge reading SHALL reflect the newly committed pages

#### Scenario: Shared regions excluded from the gauge

- **WHEN** a guest attaches shared regions
- **THEN** the shared-region bytes SHALL NOT be added to the process's linear-memory gauge

### Requirement: Peak Memory Per Billing Window

The accountant SHALL record a tenant's memory as the peak of its processes' linear-memory gauges over the billing window, not the average.

#### Scenario: Peak retained

- **WHEN** a process's memory gauge rises and then falls within a single billing window
- **THEN** the window's recorded memory SHALL be the peak reading, not the final reading

### Requirement: Tamper-Evident Metering

A process SHALL have no mechanism to decrease its own metered counters. Instruction and memory readings SHALL be computed by the host engine and SHALL be invariant under host load: the same workload SHALL meter the same instructions regardless of co-tenant activity or scheduling.

#### Scenario: Guest cannot decrement its counter

- **WHEN** a guest runs and then idles
- **THEN** its cumulative instruction counter SHALL NOT decrease

#### Scenario: Load invariance

- **WHEN** an identical workload runs on a busy host and on an idle host
- **THEN** its metered instruction count SHALL be equal

### Requirement: Per-Minute CPU Budget Enforcement

A tenant's CPU ceiling for a billing window SHALL be authored as an instruction budget. When a process's instructions consumed within the current window reaches the tenant's ceiling, the host SHALL halt the process's execution and tear it down through the standard reaping path, releasing its regions, queued pipe slots, and process-quota reservation. The ceiling SHALL be evaluated per window, so consumption in one window does not reduce the next window's ceiling. A process's budget window SHALL be aligned to the same wall-clock minute boundaries as the accountant's billing windows, and the process SHALL be bounded from its spawn rather than only from the next window boundary; a newly authored or withdrawn ceiling SHALL take effect within one host sampling interval.

#### Scenario: Over-budget process halted

- **WHEN** a process exhausts its tenant's current-window instruction budget mid-execution
- **THEN** the host SHALL halt the guest, record the exhaustion, and reap the process through the standard teardown path

#### Scenario: Budget evaluated per window

- **WHEN** a tenant was over budget in a previous window
- **THEN** the next window's ceiling SHALL be evaluated afresh rather than reduced by the previous window's consumption

#### Scenario: Budget applied at spawn

- **WHEN** a tenant with an already-authored CPU ceiling spawns a process mid-window
- **THEN** the process's engine execution budget SHALL be anchored from its spawn, without waiting for the next window boundary

#### Scenario: Window aligned to the wall-clock minute

- **WHEN** a new billing minute begins
- **THEN** each live process's budget window SHALL be re-anchored afresh at that minute boundary

### Requirement: CPU Ceiling Delivery Under a Resource Class

The per-minute CPU instruction ceiling SHALL be delivered to the runtime as an authored quota under a dedicated CPU resource class — a quota dimension, not an allocatable resource. The accountant SHALL author the ceiling under this class and the runtime SHALL read it to set per-process engine execution budgets; no resource allocation SHALL consume the class's quota.

#### Scenario: Ceiling delivered under the CPU class

- **WHEN** the accountant authors a tenant's CPU ceiling
- **THEN** it SHALL be stored as a quota for the CPU resource class and read by the runtime to set engine execution budgets

#### Scenario: CPU class allocates nothing

- **WHEN** a guest allocates or acquires any resource
- **THEN** no allocation SHALL consume the CPU resource class's quota

### Requirement: Pricing Seam

The meter SHALL report instructions and pages only. Any conversion of measured quantities into money or human units (a rate card) SHALL occur outside the meter at the pricing boundary.

#### Scenario: Meter independent of price

- **WHEN** a rate card changes
- **THEN** the metering pipeline itself SHALL be unchanged
