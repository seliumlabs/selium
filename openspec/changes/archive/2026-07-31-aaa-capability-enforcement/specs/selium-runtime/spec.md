# Spec Delta: selium-runtime

## MODIFIED Requirements

### Requirement: Grant Admission and Evaluation

`selium-runtime` SHALL reject, at spawn or `ProcessStart`, any grant
whose selectors it cannot evaluate, and SHALL evaluate every accepted
grant against authority-derived scope contexts. Empty selector lists
SHALL mean "unrestricted within the capability" and be documented as such.

#### Scenario: Accept-then-deny is impossible

- **WHEN** a guest is spawned with a grant the runtime would never be
  able to satisfy (unevaluatable selector)
- **THEN** spawning fails immediately with the selector named — the
  grant cannot enter the accept-then-always-deny state

#### Scenario: Errors attribute correctly

- **WHEN** any authorisation check fails
- **THEN** the error identifies the denied capability and the relevant
  scope values (tenant/class/identity) rather than a generic or
  misattributed capability
