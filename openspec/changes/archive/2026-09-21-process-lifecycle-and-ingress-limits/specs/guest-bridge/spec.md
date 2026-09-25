## MODIFIED Requirements

### Requirement: Bounded Spawn
The bridge-server SHALL bound bridge-channel spawns through the host-enforced per-tenant process quota: a tenant's effective spawn bound SHALL be its accountant-authored process-quota ceiling. When the runtime denies a spawn because the client tenant's process quota is exhausted, the bridge-server SHALL refuse the handoff and close the delivered stream so the client observes the refusal, and SHALL NOT maintain its own spawn counter.

#### Scenario: Spawn bound exceeded
- **WHEN** a client attempts to open more streams than the client tenant's process quota permits
- **THEN** the bridge-server SHALL refuse the additional streams and close each delivered stream so the client observes EOF

#### Scenario: Denied spawn needs no guest-local bookkeeping
- **WHEN** the runtime denies a bridge-channel spawn with `QuotaExceeded`
- **THEN** the bridge-server SHALL attach-then-close the stream and SHALL require no guest-local spawn counter to handle the refusal
