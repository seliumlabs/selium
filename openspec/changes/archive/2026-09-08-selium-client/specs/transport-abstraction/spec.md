## ADDED Requirements

### Requirement: Waker-Honest Frame Readiness
`MessageTransport` readiness over non-ring transports (for example QUIC streams) SHALL be waker-honest at the frame level: the framed read path SHALL poll the underlying `AsyncRead` with the caller's task waker, so await-based consumers park on the socket. Socket transports whose readiness is codec-driven (the framed reader's `AsyncRead` polls determine frame availability) MAY report always-ready from `poll_ready`, since polling the transport separately would register a waker the codec never uses. Transports that expose no generation counter SHALL NOT rely on the generation-wait fallback. Generation-tracked transports SHALL keep the generation-based behaviour.

#### Scenario: Socket transport parks through the codec
- **WHEN** a framed reader over a QUIC stream awaits a frame and none is ready
- **THEN** the underlying socket read SHALL be polled with the caller's task waker and the task SHALL park until a frame arrives
