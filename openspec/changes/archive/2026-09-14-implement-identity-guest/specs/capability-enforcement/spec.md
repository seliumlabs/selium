## ADDED Requirements

### Requirement: MintCertificate Is Not Conferable

`MintCertificate` SHALL be provisioned only at process bootstrap by the host. A spawn that confers `MintCertificate` on a child SHALL be denied, regardless of whether the parent holds the capability itself or a delegation scope would otherwise admit it.

#### Scenario: Providing MintCertificate at spawn is denied

- **WHEN** a process holding `MintCertificate` spawns a child whose grants include `MintCertificate`
- **THEN** the spawn SHALL be denied with a capability error
