## ADDED Requirements

### Requirement: QuotaWrite Is Not Conferable

`QuotaWrite` SHALL be provisioned only at process bootstrap by the host. A spawn that confers `QuotaWrite` on a child SHALL be denied, regardless of whether the parent holds the capability itself or a delegation scope would otherwise admit it.

#### Scenario: Conferring QuotaWrite at spawn is denied

- **WHEN** a process holding `QuotaWrite` spawns a child whose grants include `QuotaWrite`
- **THEN** the spawn SHALL be denied with a capability error
