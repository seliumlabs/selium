# Spec Delta

## ADDED Requirements

### Requirement: Framed Write Acknowledges Its Own Payload
A framed write SHALL return success only after the payload supplied to that write has been accepted; completing an older in-flight flush SHALL NOT acknowledge a newer payload.

#### Scenario: New payload follows interrupted write
- **WHEN** a previous write was cancelled during flush and the caller retries with a different payload
- **THEN** completion of the previous flush SHALL be distinguished from acceptance of the new payload and neither payload SHALL be silently lost

#### Scenario: Flush fails after interruption
- **WHEN** completion of an interrupted write fails
- **THEN** the caller SHALL receive the failure and the transport SHALL NOT report the replacement payload as sent
