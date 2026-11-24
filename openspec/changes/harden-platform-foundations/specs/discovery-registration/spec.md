# Spec Delta

## ADDED Requirements

### Requirement: Replacement Maintains Secondary Indexes
Replacing a registration or alias SHALL update its primary mapping and every label or alias reverse index as one coherent operation; obsolete index entries SHALL NOT survive replacement.

#### Scenario: Labels are removed on retargeting
- **WHEN** a URI previously labelled `app=old` is replaced by a target without that label
- **THEN** a query for `app=old` SHALL NOT return the replaced URI

#### Scenario: Alias back-reference follows replacement
- **WHEN** an alias is retargeted from canonical resource A to canonical resource B
- **THEN** revoking A SHALL NOT revoke that alias and revoking B SHALL revoke it

#### Scenario: Replacement failure leaves prior indexes coherent
- **WHEN** any part of a replacement cannot be applied
- **THEN** discovery SHALL return an error and retain either the complete old mapping or the complete new mapping, never a mixture
