## ADDED Requirements

### Requirement: Entrypoint Completion Terminates the Process
`selium-runtime` SHALL treat completion of a spawned guest's poll-owner entrypoint as the process's normal exit: when the guest's poll export reports that the entrypoint future has completed, the runtime SHALL record `ProcessExited` and run the existing process teardown — revoking the process's discovery URIs, reclaiming its regions and queued pipe slots, and releasing its process-quota reservation — rather than leaving the guest resident as an idle reactor.

#### Scenario: Completed entrypoint reaps the process
- **WHEN** a bridge-channel's pipe tears down so its entrypoint future completes
- **THEN** the runtime SHALL record `ProcessExited` and reclaim the process, its regions, and its pipe slots

#### Scenario: Long-running entrypoint stays resident
- **WHEN** a guest's poll-owner entrypoint future parks (a service loop that never returns)
- **THEN** the runtime SHALL keep the process resident and SHALL NOT reap it

### Requirement: Process Spawn Quota
`selium-runtime` SHALL enforce the tenant-scoped `ResourceClass::Process` quota when a process is spawned: a spawn whose target tenant is at its authored process ceiling SHALL be denied with `QuotaExceeded` naming the tenant and the Process dimension, before the child is instantiated. The reservation SHALL be released when the spawned process exits or is reaped. Tenant-less (root) processes SHALL NOT be metered.

#### Scenario: Spawn over the process ceiling denied
- **WHEN** the bridge-server spawns a bridge-channel for a tenant whose process quota is exhausted
- **THEN** the spawn SHALL fail with `QuotaExceeded` and no child process SHALL be created

#### Scenario: Quota distinct from lifecycle grant
- **WHEN** a process holds `ProcessLifecycle` but its target tenant is at its process ceiling
- **THEN** the spawn SHALL still be denied by the quota counter

#### Scenario: Exited process returns its slot
- **WHEN** a spawned process exits
- **THEN** its process-quota reservation SHALL return to the tenant's counter
