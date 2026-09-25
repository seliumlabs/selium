## ADDED Requirements

### Requirement: FlatBuffers Desired-State Log Records

The control plane SHALL encode the desired-state records it appends to its durable log as FlatBuffers via `FlatMsg`, and SHALL decode them with `FlatMsg` when rebuilding the projection from replay. The rkyv codec SHALL NOT be used for desired-state persistence.

#### Scenario: Deployment intent is recorded as FlatBuffers

- **WHEN** a deployment update is accepted
- **THEN** the control plane SHALL append a FlatBuffers-encoded `DesiredStateRecord::Deployment(..)` payload to the durable log

#### Scenario: Projection rebuilds from FlatBuffers replay

- **WHEN** the control plane replays the durable log on boot
- **THEN** it SHALL decode each record payload with `FlatMsg` and apply it to the projection
- **AND** a record that fails FlatBuffers decode SHALL be skipped and logged, never silently reinterpreted

#### Scenario: Desired-state types live in the service crate

- **WHEN** the control plane constructs or matches `DesiredStateRecord`, `Deployment`, or `PipelineBinding`
- **THEN** it SHALL import those types from `selium-service`, not `selium-abi`
