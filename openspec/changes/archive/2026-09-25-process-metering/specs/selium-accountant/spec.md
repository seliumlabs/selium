# Spec Delta

## MODIFIED Requirements

### Requirement: Counter and Gauge Reduction

The bookkeeper SHALL difference cumulative counters (`cpu_instructions`, `bandwidth_bytes`) against its retained last-known value per process and SHALL sample gauges (`memory_bytes`, `storage_bytes`) directly. Reduced values SHALL be summed into per-tenant buckets.

#### Scenario: Counter difference

- **WHEN** a process's cumulative cpu or bandwidth counter increases between ticks
- **THEN** the tenant bucket SHALL gain only the delta since the previous tick

#### Scenario: Gauge sample

- **WHEN** a process's memory or storage reading changes
- **THEN** the tenant bucket SHALL reflect the current reading

### Requirement: Metering Projection by the Host

The host SHALL project per-process metering observations on the sampling cadence — cumulative instructions for cpu, committed linear-memory pages for memory, current for storage, and cumulative for bandwidth — so bookkeeper reads reflect fresh consumption. The projection is driven by a one-second host ticker started at bootstrap; instruction counts and committed linear-memory pages SHALL come from the engine's per-instance metering, and bandwidth is instrumented on the TCP send/recv paths (UDP and QUIC transports are follow-ups).

#### Scenario: Observations track consumption

- **WHEN** a process executes instructions, grows linear memory, or moves bandwidth over an instrumented transport
- **THEN** projected observations SHALL reflect that consumption on the next tick

## ADDED Requirements

### Requirement: Per-Tenant CPU Budget Authoring

The accountant SHALL author each non-delinquent tenant's per-minute CPU instruction ceiling — the tenant's plan CPU plus its opt-in overage, converted to instructions by the pricing-boundary rate card — and SHALL republish it each minute. A delinquent tenant's CPU ceiling SHALL be zeroed with its other quotas.

#### Scenario: Ceiling authored per minute

- **WHEN** a tenant in good standing has a plan and an overage budget
- **THEN** the accountant SHALL author a CPU instruction ceiling equal to the plan plus the overage for the current minute

#### Scenario: Delinquent CPU ceiling zeroed

- **WHEN** a tenant transitions to delinquent
- **THEN** the accountant SHALL zero its CPU ceiling along with its other quotas
