# Spec Delta

## ADDED Requirements

### Requirement: Policy Acknowledgement Requires Durable Commit
The accountant SHALL acknowledge a policy mutation only after its ledger append and required sync succeed; a failed durable write SHALL NOT be projected or reported as updated enforcement state.

#### Scenario: Ledger append fails
- **WHEN** an accountant control mutation cannot be appended or synced
- **THEN** the caller SHALL receive an error and the previous projected policy SHALL remain authoritative

#### Scenario: Projection or enforcement fails after commit
- **WHEN** durable commit succeeds but projection or host enforcement cannot be applied
- **THEN** the accountant SHALL surface the incomplete application and reconcile the committed intent before acknowledging success
