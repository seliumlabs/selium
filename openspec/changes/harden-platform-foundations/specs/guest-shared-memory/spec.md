# Spec Delta

## ADDED Requirements

### Requirement: Safe Atomic Access Requires Valid Alignment
Safe mapping APIs SHALL validate effective-address alignment before every atomic operation and SHALL use backing storage whose alignment satisfies the operation; invalid layout SHALL return an error rather than invoke undefined behaviour.

#### Scenario: Misaligned public atomic call
- **WHEN** a caller requests a 64-bit atomic at an address not aligned to 64 bits
- **THEN** the mapping SHALL return `InvalidLayout` without performing an atomic access

#### Scenario: Heap-backed atomic operations
- **WHEN** aligned atomics operate on any safe heap-backed mapping
- **THEN** their backing storage SHALL meet the required alignment for the full mapping lifetime

### Requirement: Multi-Memory Layout Is Validated Before Allocation
Multi-memory parsing SHALL bound the entry count by the containing mapping and validate table arithmetic, entry ranges, capacity, and overlap before allocating or constructing sub-mappings.

#### Scenario: Hostile entry count
- **WHEN** a header declares more entries than its mapping can contain
- **THEN** parsing SHALL fail promptly with `InvalidLayout` and SHALL NOT allocate from the declared count

#### Scenario: Invalid entry geometry
- **WHEN** an entry range overflows, exceeds the mapping, or overlaps forbidden coordination data
- **THEN** parsing or attachment SHALL fail without creating a sub-mapping
