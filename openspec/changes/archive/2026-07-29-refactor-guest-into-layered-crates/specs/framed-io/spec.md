## MODIFIED Requirements

### Requirement: FramedRead Type
`selium-wire` SHALL provide a `FramedRead<M>` type that wraps any `MessageTransport` to provide frame-level read operations with `FrameHeader` decoding and tag extraction. `FramedRead` SHALL be generic over `M: MessageTransport` rather than over a `FrameRead` implementor. The `MessageTransport` trait provides `poll_ready()`, `generation()`, and async read methods, enabling `FramedRead` to work over shared-memory rings, QUIC streams, or any other transport without frame-level coupling.

#### Scenario: Read a complete frame
- **WHEN** a caller invokes `FramedRead::read_frame()` and a frame with the READY flag set is available on the underlying transport
- **THEN** the method SHALL return `Ok((payload_bytes, tag))` where `payload_bytes` is the frame payload and `tag` is the frame's correlation tag

#### Scenario: No frame ready
- **WHEN** a caller invokes `FramedRead::read_frame()` and no complete frame is available
- **THEN** the method SHALL return `Err(Error::BufferEmpty)`

#### Scenario: Underlying transport returns overwrite error
- **WHEN** a caller invokes `FramedRead::read_frame()` and the transport's read returns an overwrite/lag error
- **THEN** `FramedRead` SHALL propagate the error as `Error::Overwritten`

### Requirement: FramedWrite Type
`selium-wire` SHALL provide a `FramedWrite<M>` type that wraps any `MessageTransport` to provide frame-level write operations with `FrameHeader` encoding. `FramedWrite` SHALL be generic over `M: MessageTransport`.

#### Scenario: Write a framed payload
- **WHEN** a caller invokes `FramedWrite::write_frame(payload, tag)` with a payload and correlation tag
- **THEN** the method SHALL encode a `FrameHeader` with the given tag and READY flag, write the frame to the underlying transport, and return `Ok(())`

#### Scenario: Payload too large for frame
- **WHEN** a caller invokes `FramedWrite::write_frame(payload, tag)` with a payload whose length exceeds `u32::MAX`
- **THEN** the method SHALL return `Err(Error::InvalidFrame)`

### Requirement: FramedRead/FramedWrite Generic Over Transport
`FramedRead<M>` and `FramedWrite<M>` SHALL be generic over the transport type `M: MessageTransport`, allowing composition with `ShmTransport`, `QuicTransport`, or any other `MessageTransport` implementor.

#### Scenario: FramedRead wraps an shm transport
- **WHEN** a `FramedRead<ShmTransport>` is constructed
- **THEN** `read_frame()` SHALL read frames from the shared-memory ring buffer

#### Scenario: FramedRead wraps a QUIC transport
- **WHEN** a `FramedRead<QuicTransport>` is constructed
- **THEN** `read_frame()` SHALL read frames from a QUIC stream

### Requirement: FramedRead exposes generation counter
`FramedRead<M>` SHALL expose a `generation()` method that delegates to the transport's `generation()` method, enabling overwrite detection by higher-level types such as `Subscriber` when the transport supports it.

#### Scenario: Read generation counter
- **WHEN** a caller invokes `framed_read.generation()`
- **THEN** the method SHALL return the current generation counter value from the underlying transport, or zero if unsupported

### Requirement: MessageTransport Composes AsyncRead/AsyncWrite
`MessageTransport` SHALL compose `tokio::io::AsyncRead + AsyncWrite` so that existing `FrameHeader` codec infrastructure reuses standard byte-stream I/O traits without reinventing them.

#### Scenario: Frame codec reads header via AsyncRead
- **WHEN** `FramedRead` reads a `FrameHeader` from the transport
- **THEN** it SHALL use `AsyncRead::poll_read` to read the fixed-size header prefix

#### Scenario: Frame codec writes payload via AsyncWrite
- **WHEN** `FramedWrite` writes a frame to the transport
- **THEN** it SHALL use `AsyncWrite::poll_write` to write the header and payload bytes
