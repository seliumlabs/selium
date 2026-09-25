## Purpose

The accounting guest reduces host-level metering into per-tenant usage, bills that usage against plan and overage budgets over per-minute windows, and authors the quota, narrowing, and rate-limit state that the host and edge enforce preemptively.

## ADDED Requirements

### Requirement: Per-Second Metering Sampling

The bookkeeper entrypoint SHALL sample host-local metering observations for every live local process once per second via `MeteringRead`. The bookkeeper SHALL derive the live process set from lifecycle activity events and map each process to its tenant via `ProcessTenant`.

#### Scenario: Sample covers all live processes

- **WHEN** the bookkeeper ticks each second
- **THEN** it SHALL read the current metering observation for each live local process

### Requirement: Counter and Gauge Reduction

The bookkeeper SHALL difference cumulative counters (`cpu_micros`, `bandwidth_bytes`) against its retained last-known value per process and SHALL sample gauges (`memory_bytes`, `storage_bytes`) directly. Reduced values SHALL be summed into per-tenant buckets.

#### Scenario: Counter difference

- **WHEN** a process's cumulative cpu or bandwidth counter increases between ticks
- **THEN** the tenant bucket SHALL gain only the delta since the previous tick

#### Scenario: Gauge sample

- **WHEN** a process's memory or storage reading changes
- **THEN** the tenant bucket SHALL reflect the current reading

### Requirement: Bucket Publication

The bookkeeper SHALL publish per-tenant metering buckets to a shared-memory topic consumed by the accountant entrypoint. On a single host the topic SHALL be host-local shared memory; cross-host transport SHALL be deferred.

#### Scenario: Accountant consumes buckets

- **WHEN** the accountant merges published bucket messages
- **THEN** it SHALL observe each tenant's usage for the sampling interval

### Requirement: Metering Projection by the Host

The host SHALL project per-process metering observations on the sampling cadence — cumulative for cpu and bandwidth, current for memory and storage — so bookkeeper reads reflect fresh consumption. The projection is driven by a one-second host ticker started at bootstrap; bandwidth is instrumented on the TCP send/recv paths (UDP and QUIC transports are follow-ups); per-process CPU accounting is a host instrumentation hook until the WASM-resources change lands.

#### Scenario: Observations track consumption

- **WHEN** processes consume storage, or bandwidth over an instrumented transport
- **THEN** projected observations SHALL reflect that consumption on the next tick

### Requirement: Per-Minute Billing Windows and Ledger

The accountant SHALL merge bookkeeper buckets and roll them into per-minute billing windows, appending each window's per-tenant usage to a durable ledger. A bucket SHALL be attributed to the billing window of its bookkeeper publish-time stamp (carried on the bucket), not the accountant's receive-time clock. A lost single-second sample SHALL be recovered by cumulative-counter difference over the window, bounded by one sampling interval.

#### Scenario: Window rolled to ledger

- **WHEN** a minute completes
- **THEN** the accountant SHALL append a per-tenant ledger row carrying windowed usage per dimension

#### Scenario: Dropped sample recovered

- **WHEN** one second's sample is lost mid-window
- **THEN** the window's counter delta SHALL still include the lost consumption within one sampling interval

### Requirement: Plan and Overage Ceilings

The accountant SHALL evaluate each tenant's windowed usage against a soft ceiling (the paid plan) and a hard ceiling (plan plus an opt-in overage budget). Usage above the soft ceiling SHALL be recorded as billable overage. Usage SHALL NOT pass the hard ceiling because the chokepoints preempt it.

#### Scenario: Overage recorded

- **WHEN** a tenant's windowed usage exceeds the plan but not the hard ceiling
- **THEN** the ledger SHALL record the excess as billable overage

#### Scenario: Hard ceiling preempted

- **WHEN** a tenant's usage reaches the hard ceiling
- **THEN** the allocation and rate chokepoints SHALL already deny or throttle further consumption

### Requirement: Account State Machine and Enforcement

The accountant SHALL maintain per-tenant account state — paid, in-overage, at-budget, delinquent — and SHALL translate state transitions into enforcement state: quota values via `QuotaSet` and a published narrowing set. (Bandwidth rate bands and pricing are operator-authored policy whose authoring surface — cloud management tooling — is deferred; the accountant does not author them.) A delinquent tenant SHALL have its narrowing set to nothing and its quotas set to zero.

#### Scenario: Delinquency suspends a tenant

- **WHEN** a tenant transitions to delinquent
- **THEN** the accountant SHALL narrow its grants to nothing and zero its quotas

#### Scenario: Restored tenant

- **WHEN** a tenant returns to good standing
- **THEN** the accountant SHALL restore its quotas and narrowing

### Requirement: Quota Enforcement at Allocation

Quota counters authored via `QuotaSet` SHALL be enforced synchronously by the host at allocation points — shared-memory bytes at region allocation, storage bytes at log append and blob put, and queued pipe items at enqueue — denying an allocation that exceeds the tenant's ceiling before any resource is granted. Quotas SHALL be distinct from capability grants. Usage SHALL be tracked against the tenant's counter even before a ceiling is authored, so an accountant-authored `QuotaSet` arriving after the tenant is already running inherits its live usage. Denials SHALL fail with the `QuotaExceeded` error code, with the message naming the tenant and resource class (dimension). Reservations SHALL be released when the resource is freed or its process dies — except storage, which is sticky (durable bytes persist by design); a user-side durable-storage management mechanism, e.g. a CLI capability, is TBA.

#### Scenario: Allocation over quota denied

- **WHEN** a tenant attempts to allocate beyond its granted quota
- **THEN** the hostcall SHALL fail with the `QuotaExceeded` error code and a message naming the tenant and dimension

#### Scenario: Quota distinct from grant

- **WHEN** a tenant holds the capability grant for a resource class but lacks quota
- **THEN** the allocation SHALL still be denied by the quota counter

#### Scenario: Pre-authoring usage is inherited

- **WHEN** a tenant allocates before the accountant authors a ceiling, and a `QuotaSet` arrives afterwards
- **THEN** the authored ceiling SHALL account for the live usage allocated beforehand

#### Scenario: A dying process releases its reservations

- **WHEN** a process holding a region or queued pipe items terminates
- **THEN** the region's bytes and the queued items' pipe slots SHALL return to its tenant's quota; durable-storage bytes SHALL remain reserved

### Requirement: Pipe Quotas Meter Queued Items

The pipes dimension SHALL meter queued items, not queue count: creating a queue consumes no quota; each item enqueued onto a queue reserves one slot against the queue owner's (serving) tenant; receiving an item releases its slot; items still queued when the owner terminates are released at teardown. An over-ceiling guest send SHALL be denied with `QuotaExceeded`; an over-ceiling kernel-side delivery (e.g. an accepted connection onto a listener whose owner is at its pipe ceiling) SHALL be dropped at the chokepoint with a debug-level log naming the queue, tenant, and ceiling.

#### Scenario: Over-ceiling send denied

- **WHEN** a guest sends onto a queue whose owner's pipe quota is exhausted
- **THEN** the send SHALL fail with `QuotaExceeded` and the item SHALL NOT be queued

#### Scenario: Receiving frees the slot

- **WHEN** an item is received off a queue
- **THEN** the pipe slot it reserved SHALL be released to the queue owner's tenant

### Requirement: Handed-Off Resources Transfer Ownership

A resource delivered through a queue handoff SHALL transfer: the sender's resource-table entry moves to the receiver (the sender can no longer free it), and the region's quota reservation moves from the sender's tenant to the receiver's. The receiver's consumption SHALL be force-accepted, not denied: a peer can hand over a "poisoned" resource (e.g. a massive allocation) that pushes the receiving tenant over its ceiling — subsequent allocations are denied and metering surfaces the anomaly. This is a **known open attack vector**; denying the receive instead would require peek/requeue machinery and would clog the victim's queue slot permanently, a strictly worse denial than the documented ceiling overflow. Cross-tenant handoffs also re-key discovery revocation bookkeeping under the receiver's tenant; the tier-1 registration URI remains minted under the original tenant, so a cross-tenant revocation may miss (single-tenant handoffs — all current flows — are unaffected).

#### Scenario: Cross-tenant handoff moves the reservation

- **WHEN** a tenant hands a region to a peer of another tenant and the peer receives it
- **THEN** the region's bytes SHALL move from the sender's quota to the receiver's, and ownership (including free rights) SHALL transfer

### Requirement: Narrowing Publication

The accountant SHALL publish a per-tenant narrowing state consumed by the bridge, so conferral SHALL apply a tenant's narrowing to the identity-published baseline grants.

#### Scenario: Narrowed conferral

- **WHEN** the bridge confers grants for a narrowed tenant
- **THEN** the spawned process SHALL receive the baseline grants minus the narrowing

### Requirement: Sole Quota Authorship

The accountant SHALL be the sole author of quota counters. No other guest SHALL write them; `QuotaWrite` SHALL be bootstrap-provisioned and non-conferable.

#### Scenario: Non-accountant quota write denied

- **WHEN** a guest other than the accountant issues a quota hostcall
- **THEN** the runtime SHALL deny it with a capability error
