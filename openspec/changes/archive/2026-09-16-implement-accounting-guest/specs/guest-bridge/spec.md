## ADDED Requirements

### Requirement: Accountant Narrowing at Conferral

The bridge-server SHALL fold the accountant's published narrowing state into the identity-published baseline grants before conferring a grant set on a spawned bridge-channel.

#### Scenario: Baseline narrowed before conferral

- **WHEN** the bridge-server resolves a handoff identity and its narrowing state exists
- **THEN** the bridge-channel SHALL be spawned holding the baseline grants minus the narrowing

#### Scenario: No narrowing is a no-op

- **WHEN** a tenant has no published narrowing
- **THEN** the bridge-channel SHALL receive the identity-published baseline grants unchanged

#### Scenario: Narrowing table unreachable fails open

- **WHEN** the bridge-server boots and the accountant's narrowing table route cannot be resolved or attached (the accountant is down or booting)
- **THEN** the bridge-server SHALL continue conferring identity-published baseline grants un-narrowed (fail-open), with a warning logged — consistent with the accepted v1 single-accountant SPOF; the accountant re-publishes its narrowing state at its own boot, and conferrals fold it in from the first sync after it returns
