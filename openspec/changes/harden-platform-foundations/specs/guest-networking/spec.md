# Spec Delta

## ADDED Requirements

### Requirement: Arbitrary Frame Sizes Preserve Byte Streams
Guest byte-stream readers SHALL reassemble frame headers and payloads across partial reads for every frame size admitted by the attached ring; a zero-length frame SHALL NOT be treated as EOF.

#### Scenario: Payload exceeds 64 KiB
- **WHEN** a valid frame payload exceeds the reader's temporary buffer, including a 1 MiB ring payload
- **THEN** the reader SHALL return exactly the payload bytes as a continuous stream without parsing payload bytes as another header

#### Scenario: Header-only frame
- **WHEN** a valid frame contains no payload and the peer remains connected
- **THEN** the reader SHALL continue waiting for subsequent bytes rather than report EOF

### Requirement: Directional Ring Geometry Is Preserved
Attaching a bidirectional shared-memory stream SHALL derive inbound and outbound capacities independently from their respective layout entries.

#### Scenario: Asymmetric rings attach
- **WHEN** inbound and outbound entries declare different capacities
- **THEN** each channel SHALL enforce its own declared capacity and SHALL NOT inherit the peer direction's geometry

### Requirement: Async Shutdown Closes The Writer
Successful `AsyncWrite::poll_shutdown` SHALL perform the same writer registration release and peer EOF signalling as closing the writer, and SHALL reject later writes.

#### Scenario: Shutdown wakes peer with EOF
- **WHEN** a writer shuts down while its peer is parked on input
- **THEN** the peer SHALL wake, drain committed data, and observe EOF

#### Scenario: Write after shutdown
- **WHEN** a caller attempts to write after successful shutdown
- **THEN** the writer SHALL return an error and SHALL NOT publish another frame
