# Spec Delta

## ADDED Requirements

### Requirement: Process Metering Observation

`selium-abi` SHALL define `MeteringObservation` with fields `cpu_instructions: u64` (cumulative executed instructions), `memory_bytes: u64` (current committed linear-memory gauge), `storage_bytes: u64` (durable storage gauge), and `bandwidth_bytes: u64` (cumulative network bytes). The type SHALL NOT define a `cpu_micros` field.

#### Scenario: Observation fields round-trip

- **WHEN** a `MeteringObservation` is rkyv-encoded and decoded
- **THEN** `cpu_instructions`, `memory_bytes`, `storage_bytes`, and `bandwidth_bytes` SHALL survive unchanged

#### Scenario: Cpu microseconds absent

- **WHEN** a consumer reads a `MeteringObservation`
- **THEN** there SHALL be no `cpu_micros` field to read

### Requirement: CPU Resource Class

`selium-abi` SHALL include `Cpu` in the `ResourceClass` enum so that a tenant's per-minute CPU instruction ceiling can be carried through the shared quota vocabulary as a quota dimension (not an allocatable resource).

#### Scenario: Cpu variant joins the resource vocabulary

- **WHEN** a resource class is converted to and from its URI segment
- **THEN** `ResourceClass::Cpu` SHALL round-trip through the segment `cpu` like every other class

#### Scenario: Cpu class allocates no resource

- **WHEN** a guest allocates or acquires any resource
- **THEN** nothing SHALL consume a `ResourceClass::Cpu` allocation, because the class only carries the CPU instruction ceiling
