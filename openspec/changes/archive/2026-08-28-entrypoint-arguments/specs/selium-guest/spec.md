## ADDED Requirements

### Requirement: Entrypoint Argument Reader

`selium-guest` SHALL provide a helper that reconstructs a slice of bytes
from a `(u64, u64)` pointer argument, so guest entrypoints do not repeat
unsafe slice construction. The helper is `unsafe` (the address and length
are trusted only because the runtime wrote them) and provides a UTF-8
variant for string configuration.

#### Scenario: Read pointer argument as bytes

- **WHEN** a guest receives a `(u64, u64)` pointer argument written by the runtime
- **THEN** `selium_guest::args::bytes(ptr, len)` SHALL return a byte slice over the payload

#### Scenario: Read pointer argument as UTF-8

- **WHEN** a guest receives a `(u64, u64)` pointer argument carrying a valid UTF-8 payload
- **THEN** `selium_guest::args::str(ptr, len)` SHALL return `Some(&str)` for valid UTF-8 and `None` otherwise
