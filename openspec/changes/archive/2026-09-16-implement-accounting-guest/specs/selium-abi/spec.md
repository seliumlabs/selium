## ADDED Requirements

### Requirement: Quota Hostcall Variants

`HostcallRequest` SHALL define `QuotaSet { tenant, class, limit }` and `QuotaClear { tenant, class }`, rkyv-encoded like all hostcall requests. `QuotaSet` SHALL store a quota counter for the tenant and resource class; `QuotaClear` SHALL remove it. The corresponding `HostcallOutput` SHALL return unit. A quota denial (an allocation or enqueue that would exceed the tenant's authored ceiling) SHALL fail with the `AbiErrorCode::QuotaExceeded` error code — distinct from `PermissionDenied` — so guests can tell "you lack the capability" apart from "your tenant hit its ceiling"; the message SHALL name the tenant and resource class.

#### Scenario: QuotaSet round-trips

- **WHEN** a guest encodes `HostcallRequest::QuotaSet` with a tenant, resource class, and limit
- **THEN** the hostcall SHALL complete and the host SHALL store the counter

#### Scenario: QuotaClear removes the counter

- **WHEN** a guest encodes `HostcallRequest::QuotaClear` for a tenant and class
- **THEN** the hostcall SHALL complete and the stored counter SHALL be removed

### Requirement: QuotaWrite Capability Variant

`Capability` SHALL include a `QuotaWrite` variant that gates the quota hostcalls. The variant SHALL be rkyv-encodable like the other capability variants.

#### Scenario: Quota hostcall requires the capability

- **WHEN** a process without a `QuotaWrite` grant invokes a quota hostcall
- **THEN** the runtime SHALL deny the hostcall with a capability error
