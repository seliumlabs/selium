## MODIFIED Requirements

### Requirement: Channel creation with backpressure
`Channel` SHALL provide `create(capacity: u64, backpressure: ChannelBackpressure) -> Result<Self>` where `ChannelBackpressure` is an enum with `Park` (default for backward compatibility) and `Drop` variants. On `Drop` channels, `blocking_writer()` SHALL return `Error::BackpressureNotSupported`. On `Park` channels, behaviour is unchanged.

#### Scenario: Channel created with Park backpressure
- **WHEN** a caller invokes `Channel::create(65536, ChannelBackpressure::Park)`
- **THEN** `blocking_writer()` SHALL succeed and writers SHALL respect blocking reader positions

#### Scenario: Channel created with Drop backpressure
- **WHEN** a caller invokes `Channel::create(65536, ChannelBackpressure::Drop)`
- **THEN** `writer()` SHALL succeed with a non-blocking writer
- **AND** `blocking_writer()` SHALL return `Err(Error::BackpressureNotSupported)`

#### Scenario: Drop channel writer never blocks on readers
- **WHEN** a writer on a Drop channel writes faster than a blocking reader consumes
- **THEN** the writer SHALL never return `Poll::Pending` due to reader backpressure
- **AND** the reader MAY receive `Error::Overwritten` when it falls behind

### Requirement: Channel backpressure error variant
`selium_guest::io::Error` SHALL include a `BackpressureNotSupported` variant returned when `blocking_writer()` is called on a `Drop`-backpressure channel.

#### Scenario: BackpressureNotSupported error on Drop channel
- **WHEN** a caller invokes `channel.blocking_writer()` on a Drop channel
- **THEN** the method SHALL return `Err(Error::BackpressureNotSupported)`

### Requirement: Strong Writer Backpressure via AsyncWrite
`Writer` SHALL implement `tokio::io::AsyncWrite`. The `poll_write` method SHALL write bytes as a framed payload to the outbound ring buffer. On `Park` channels, `protect_readers` SHALL be `true`; on `Drop` channels, `protect_readers` SHALL be `false`. If the buffer is full on a `Park` channel, `poll_write` SHALL return `Poll::Pending`.

#### Scenario: Writer sends bytes on Drop channel
- **WHEN** a caller invokes `poll_write(buf)` on a `Writer` from a Drop channel
- **THEN** the method SHALL reserve space with `protect_readers = false` and never return `Poll::Pending` due to reader backpressure

### Requirement: RingBuf creation with ResourceKind
`RingBuf::create` SHALL accept `capacity: u64` and `purpose: ResourceKind` and SHALL thread the purpose through to the underlying `AllocRegion { purpose }` hostcall.

#### Scenario: RingBuf created with LogChannel purpose
- **WHEN** a caller invokes `RingBuf::create(65536, ResourceKind::LogChannel)`
- **THEN** the `AllocRegion` hostcall SHALL carry `purpose: ResourceKind::LogChannel`
