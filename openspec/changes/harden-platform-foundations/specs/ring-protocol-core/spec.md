# Spec Delta

## ADDED Requirements

### Requirement: Wrappers Cannot Diverge From Canonical Validation
Public ring readers, writers, and compatibility wrappers SHALL delegate frame sizing, reservation, encoding, decoding, and validation to the canonical protocol primitives rather than maintain parallel rules.

#### Scenario: Oversized encoded frame rejected uniformly
- **WHEN** any public reader encounters a header whose encoded frame exceeds ring capacity
- **THEN** it SHALL return the same canonical invalid-frame error as every other reader

#### Scenario: Encoded buffer length disagrees with header
- **WHEN** a writer receives an encoded buffer whose actual length differs from its header's declared frame length
- **THEN** it SHALL reject the buffer before publishing a ready frame

#### Scenario: Protocol regression applies to all wrappers
- **WHEN** a canonical frame-validation rule changes
- **THEN** conformance tests SHALL demonstrate identical behaviour through layout primitives, ring buffers, and channel readers/writers
