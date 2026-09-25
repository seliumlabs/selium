## MODIFIED Requirements

### Requirement: Quota Enforcement at Allocation

Quota counters authored via `QuotaSet` SHALL be enforced synchronously by the host at allocation points — shared-memory bytes at region allocation, storage bytes at log append and blob put, queued pipe items at enqueue, and process count at process spawn — denying an allocation or spawn that exceeds the tenant's ceiling before any resource is granted or process is created. Quotas SHALL be distinct from capability grants. Usage SHALL be tracked against the tenant's counter even before a ceiling is authored, so an accountant-authored `QuotaSet` arriving after the tenant is already running inherits its live usage. Denials SHALL fail with the `QuotaExceeded` error code, with the message naming the tenant and resource class (dimension). Reservations SHALL be released when the resource is freed or its process dies — except storage, which is sticky (durable bytes persist by design); a user-side durable-storage management mechanism, e.g. a CLI capability, is TBA.

#### Scenario: Allocation over quota denied

- **WHEN** a tenant attempts to allocate beyond its granted quota
- **THEN** the hostcall SHALL fail with the `QuotaExceeded` error code and a message naming the tenant and dimension

#### Scenario: Spawn over process quota denied

- **WHEN** a tenant attempts to spawn a process beyond its process-quota ceiling
- **THEN** the spawn SHALL fail with the `QuotaExceeded` error code and a message naming the tenant and the Process dimension

#### Scenario: Quota distinct from grant

- **WHEN** a tenant holds the capability grant for a resource class but lacks quota
- **THEN** the allocation SHALL still be denied by the quota counter

#### Scenario: Pre-authoring usage is inherited

- **WHEN** a tenant allocates before the accountant authors a ceiling, and a `QuotaSet` arrives afterwards
- **THEN** the authored ceiling SHALL account for the live usage allocated beforehand

#### Scenario: A dying process releases its reservations

- **WHEN** a process holding a region, queued pipe items, or a process slot terminates
- **THEN** the region's bytes, the queued items' pipe slots, and the process slot SHALL return to its tenant's quota; durable-storage bytes SHALL remain reserved

## ADDED Requirements

### Requirement: Process Quota Ceiling

The accountant SHALL author a per-tenant process-quota ceiling via `QuotaSet` for `ResourceClass::Process`, with a default of 100 processes per tenant that operators MAY raise per tenant on request via the `AccountantControl::SetProcessQuota` control message (replayed through the ledger like `SetPlan`/`SetOverage`). A delinquent tenant's process ceiling SHALL be zeroed with its other quotas.

#### Scenario: Default process ceiling authored

- **WHEN** a tenant in good standing has no operator-authored process ceiling
- **THEN** the accountant SHALL author a process-quota ceiling of 100 for that tenant

#### Scenario: Raised process ceiling authored

- **WHEN** an operator raises a tenant's process ceiling via a `SetProcessQuota` control message
- **THEN** the accountant SHALL record the raise in its ledger and author the raised ceiling via `QuotaSet` for `ResourceClass::Process`

#### Scenario: Delinquent process ceiling zeroed

- **WHEN** a tenant transitions to delinquent
- **THEN** the accountant SHALL zero its process-quota ceiling along with its other quotas
