## ADDED Requirements

### Requirement: Waker-Honest Async Read Path
`FramedRead` SHALL expose a waker-honest async read path (`poll_frame` / `read_frame_async`) that polls the underlying transport's `AsyncRead` with the caller's task waker. Transports that expose no generation counter SHALL be read through this path, so await-based native consumers park on the socket. Generation-tracked (shared-memory) readers SHALL retain the existing generation-based read behaviour.

#### Scenario: Native read parks instead of spinning
- **WHEN** a `Subscriber` over a transport with no generation counter awaits the next message and no frame is ready
- **THEN** the task SHALL park on the transport's read waker until a frame arrives

#### Scenario: Generation-tracked read is unchanged
- **WHEN** a `FramedRead` drives a generation-tracked (shared-memory) transport
- **THEN** it SHALL retain the existing generation-based read behaviour
