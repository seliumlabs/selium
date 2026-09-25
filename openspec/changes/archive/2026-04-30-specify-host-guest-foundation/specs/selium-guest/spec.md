## ADDED Requirements

### Requirement: Safe Guest Handles
`selium-guest` SHALL provide safe, ergonomic handle types over ABI primitives so guest code does not manipulate raw hostcall payloads directly for common operations.

#### Scenario: Guest opens primitive through SDK handle
- **WHEN** guest code acquires a storage, network, or shared-memory resource through the SDK
- **THEN** the SDK SHALL expose a typed handle rather than requiring direct ABI framing code

### Requirement: Messaging-Pattern Layer
`selium-guest` SHALL provide a messaging-pattern layer built above the primitive substrate.

#### Scenario: Guest selects messaging pattern
- **WHEN** guest code needs pub/sub, fanout, request/reply, stream, or live-table semantics
- **THEN** the SDK SHALL provide those semantics through the pattern layer rather than through guest-specific boilerplate

#### Scenario: Prototype-local pattern composition
- **WHEN** the current arch3 prototype uses the messaging-pattern layer in native tests or single-process guest logic
- **THEN** the SDK MAY satisfy those semantics through local in-memory composition while the host-backed inter-guest fabric remains future work

### Requirement: Pattern Parity
`selium-guest` SHALL treat request/reply as one messaging pattern among peers and SHALL NOT require RPC-style APIs as the privileged default for inter-guest communication.

#### Scenario: Pub/sub without RPC wrapper
- **WHEN** a guest uses pub/sub semantics for coordination
- **THEN** the SDK SHALL support that pattern directly without requiring the guest to model the interaction as request/reply first

### Requirement: Typed Codec Support
`selium-guest` SHALL provide typed codecs that map guest data types onto canonical ABI framing rules.

#### Scenario: Typed payload round trip
- **WHEN** guest code sends and receives a typed payload through the SDK
- **THEN** the SDK SHALL encode and decode it using the canonical ABI contract

### Requirement: Tracing and Log Integration
`selium-guest` SHALL integrate guest tracing with guest-visible log resources so that structured logs can be emitted without host-specific application code.

#### Scenario: Guest emits structured log event
- **WHEN** guest code emits a tracing event through the SDK
- **THEN** the SDK SHALL forward that event to the configured guest-visible log resource

### Requirement: Native Test Fallbacks
`selium-guest` SHALL support native testing fallbacks for guest code where practical so pattern and handle behaviour can be validated outside Wasm-only execution.

#### Scenario: Native test exercises guest pattern code
- **WHEN** a guest module is tested on a native target using the SDK's fallback path
- **THEN** the test SHALL be able to validate guest logic without requiring full Wasm deployment
