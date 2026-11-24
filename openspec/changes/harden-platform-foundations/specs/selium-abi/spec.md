# Spec Delta

## MODIFIED Requirements

### Requirement: Sleep Hostcall Variant
`HostcallRequest` SHALL include a `Sleep { millis: u64 }` variant. The runtime SHALL compute the deadline with checked arithmetic and store a valid operation in `HostOperationState::SleepWait`; an unrepresentable deadline SHALL fail with `MalformedPayload` without panicking. Polling SHALL complete only at or after the deadline.

#### Scenario: Sleep operation created
- **WHEN** the runtime dispatches `HostcallRequest::Sleep { millis: 500 }`
- **THEN** the operation SHALL have a deadline at least 500ms after the current monotonic reading

#### Scenario: Sleep operation polled before deadline
- **WHEN** a `SleepWait` operation is polled before its deadline
- **THEN** the poll SHALL return pending

#### Scenario: Sleep operation polled after deadline
- **WHEN** a `SleepWait` operation is polled at or after its deadline
- **THEN** it SHALL return `HostcallOutput::Empty`

#### Scenario: Sleep deadline is unrepresentable
- **WHEN** `millis` cannot be added to the host's current monotonic instant
- **THEN** creation SHALL fail with `MalformedPayload` and no timer or host panic SHALL occur

## ADDED Requirements

### Requirement: Hostcall Envelope Has A Bounded Representation
The host SHALL reject an envelope whose declared byte length exceeds a defined protocol maximum or whose memory range is invalid before allocating a decoding buffer.

#### Scenario: Declared length exceeds guest memory
- **WHEN** a guest supplies an envelope pointer and length outside its exported memory
- **THEN** the host SHALL return a malformed-request failure without allocating the declared length

#### Scenario: Declared length exceeds protocol maximum
- **WHEN** a guest supplies an otherwise valid range larger than the hostcall envelope maximum
- **THEN** the host SHALL reject it before decoding and resource usage SHALL remain bounded

### Requirement: Host And Guest Negotiate ABI Compatibility
Every host/guest session SHALL exchange an explicit ABI version before dispatching requests; incompatible versions SHALL fail loudly before any request is interpreted.

#### Scenario: Current versions match
- **WHEN** host and guest advertise the same supported ABI version
- **THEN** request dispatch SHALL proceed normally

#### Scenario: Guest ABI is incompatible
- **WHEN** a guest built against an unsupported older or newer ABI connects to the host
- **THEN** bootstrap SHALL fail with an incompatibility error naming both versions and SHALL NOT decode guest requests as another variant layout
