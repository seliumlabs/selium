# Spec Delta

## ADDED Requirements

### Requirement: Concurrent Volume Appends Have Unique Ordered Offsets
Appends to one volume SHALL be serialised across every open handle, and each successful append SHALL return the unique starting offset of its complete byte range.

#### Scenario: Two handles append concurrently
- **WHEN** two independently opened handles for one volume append different bytes concurrently
- **THEN** their returned ranges SHALL NOT overlap and reading the volume SHALL return both complete payloads in offset order

#### Scenario: Partial append failure
- **WHEN** an append cannot write its complete payload
- **THEN** the kernel SHALL report failure and recovery SHALL NOT interpret a partial payload as a committed append
