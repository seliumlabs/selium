# Spec Delta

## ADDED Requirements

### Requirement: Sleep Requests Have Bounded Deadline Arithmetic
The host SHALL use checked monotonic deadline arithmetic for every sleep request and SHALL reject durations it cannot represent rather than panic or wrap.

#### Scenario: Maximum duration is hostile
- **WHEN** a guest requests `Sleep { millis: u64::MAX }`
- **THEN** the host SHALL return `MalformedPayload`, allocate no timer, and continue serving other guests

#### Scenario: Maximum accepted duration
- **WHEN** a guest requests the greatest representable supported sleep duration
- **THEN** the operation SHALL remain pending until its valid deadline or cancellation and SHALL NOT complete early
