# Spec Delta: selium-kernel

## MODIFIED Requirements

### Requirement: Kernel Consumes the Shared Ring Implementation

The kernel SHALL use the shared ring protocol implementation for network
proxies and guest log drains. Bespoke frame codecs, reservation logic,
slot scans, and multi-memory header handling SHALL NOT exist in the
kernel.

#### Scenario: Network proxy uses shared primitives

- **WHEN** the kernel proxies a TCP/UDP stream to or from a guest ring
- **THEN** frame reads/writes, reservations, and reader-slot updates go
  through the shared ring primitives, not kernel-local copies

#### Scenario: Log drain uses shared frame reader

- **WHEN** the kernel drains a guest log channel
- **THEN** it reads frames with the shared frame reader and ring geometry
  from the channel header, with no local frame parsing

## REMOVED Requirements

### Requirement: Kernel-Local Ring Protocol

**Reason**: superseded by the shared ring protocol core; duplicated logic
was the source of shipped geometry and atomicity bugs.

**Migration**: all kernel ring consumers move to the shared primitives;
the kernel-local helpers are deleted, not deprecated.
