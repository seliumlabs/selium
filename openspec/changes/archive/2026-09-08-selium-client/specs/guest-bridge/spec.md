## MODIFIED Requirements

### Requirement: Typed Pipe Handshake
Before relaying data frames, a bridge-channel SHALL read a typed handshake message naming the fabric channel to bridge (its discovery URI). The handshake SHALL be deterministic: after reading the handshake, the bridge-channel SHALL send exactly one typed control reply — an acceptance frame once the channel is resolved and attached (immediately before the relay begins), or a termination frame describing the failure (followed by stream teardown). Data frames SHALL be relayed only after the acceptance reply.

#### Scenario: Client opens a pipe
- **WHEN** an external client sends a handshake message naming a channel URI on a stream
- **THEN** the bridge-channel SHALL resolve and attach that channel, reply with an acceptance control frame, and only then relay further frames

#### Scenario: Pipe instantiation fails
- **WHEN** the bridge-channel cannot resolve or attach the requested channel
- **THEN** it SHALL send a typed termination message describing the failure and close the stream
