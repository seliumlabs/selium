## MODIFIED Requirements

### Requirement: Safe Guest Handles
`selium-guest` SHALL be the WASM guest SDK, providing safe, ergonomic handle types for the host ABI and re-exporting the lower crates (`selium-wire`, `selium-shm`, `selium-encoding`, `selium-memory`) so that guest code sees a single unified API surface. Handles SHALL include `Reader`, `Writer`, `FramedRead`, `FramedWrite`, `Subscriber<T>`, `Publisher<T>`, `RpcClient<Req, Rep>`, `RpcConnection<Req, Rep>`, `LiveTable<K, V>`, and `LogRecord`/`LogField`/`LogSpan`/`LogLevel`.

#### Scenario: Guest opens primitive through SDK handle
- **WHEN** guest code acquires a shared memory, channel, or pub/sub resource through the SDK
- **THEN** the SDK SHALL expose a typed handle rather than requiring direct ABI framing code

### Requirement: Messaging-Pattern Layer
`selium-guest` SHALL re-export the messaging-pattern layer from `selium-wire` (pub/sub, RPC, live tables). The pattern layer SHALL be generic over `MessageTransport`. For WASM guests, `selium-guest` SHALL install the `ShmTransport` backed by the hostcall `RegionProvider` as the default transport.

#### Scenario: Guest selects messaging pattern
- **WHEN** guest code needs pub/sub, fanout, request/reply, stream, or live-table semantics
- **THEN** the SDK SHALL provide those semantics through the re-exported `selium-wire` pattern types

### Requirement: Pub/Sub Generation-Change Detection
`Subscriber<T, M>` SHALL detect when the publisher has overwritten unread data. For transports that support generation tracking (e.g., `ShmTransport`), detection SHALL use the generation counter delta. For transports that do not (e.g., `QuicTransport`), overwrite SHALL NOT be reported.

#### Scenario: Publisher overwrites unread data (shm)
- **WHEN** a subscriber calls `Stream::poll_next()` over shm and the underlying `Reader::read_frame` returns an overwrite error
- **THEN** the subscriber SHALL surface `Error::Overwritten` through the stream

#### Scenario: Normal publishing within capacity
- **WHEN** a subscriber calls `Stream::poll_next()` and the generation counter delta is within capacity
- **THEN** the subscriber SHALL read the next available frame normally

### Requirement: Non-Blocking Reader Poll
`Reader` and `WeakReader` SHALL implement `tokio::io::AsyncRead`. `selium-shm`'s `ShmTransport` SHALL wrap these to implement `MessageTransport`.

#### Scenario: Frame is ready
- **WHEN** a caller invokes `poll_ready()` on an `ShmTransport` and a frame with the READY flag is at the current read position
- **THEN** the method SHALL return `Ok(true)`

#### Scenario: No frame ready, writers connected
- **WHEN** a caller invokes `poll_read()` and no frame is available but writer_count > 0
- **THEN** the transport SHALL return `Poll::Pending`

#### Scenario: No frame ready, all writers disconnected
- **WHEN** a caller invokes `poll_peer_closed()` and writer_count == 0
- **THEN** the method SHALL return `Ok(true)`

### Requirement: Reader/Writer Upgrade and Downgrade
`Reader` SHALL provide `downgrade(self) -> WeakReader` that releases the reader slot. `WeakReader` SHALL provide `upgrade(self) -> Result<Reader>`. `Writer` SHALL provide `downgrade(self) -> WeakWriter`; `WeakWriter` SHALL provide `upgrade(self) -> Result<Writer>`.

#### Scenario: Strong reader downgrades to weak
- **WHEN** a caller invokes `reader.downgrade()`
- **THEN** the reader slot SHALL be released and a `WeakReader` at the same position SHALL be returned

#### Scenario: Weak reader upgrades to strong
- **WHEN** a caller invokes `weak_reader.upgrade()`
- **THEN** a new reader slot SHALL be allocated at the current position and a `Reader` SHALL be returned

### Requirement: Subscriber/Publisher Upgrade and Downgrade
`Subscriber<T, M>` and `Publisher<T, M>` SHALL support upgrade/downgrade when `M = ShmTransport` by delegating to the inner transport's strong/weak conversion.

#### Scenario: Subscriber upgrades from weak to strong (shm)
- **WHEN** a caller invokes `subscriber.upgrade()` on an shm-backed subscriber using a weak reader
- **THEN** a new `Subscriber<T, ShmTransport>` SHALL be returned with strong-reader backpressure

### Requirement: LiveTable in selium-wire
`LiveTable<K, V>` SHALL reside in `selium-wire::tables` as the single canonical implementation, generic over `MessageTransport`.

#### Scenario: LiveTable is importable from selium-wire
- **WHEN** a crate imports `selium_wire::tables::LiveTable`
- **THEN** the import SHALL resolve to the canonical `LiveTable` type

### Requirement: RPC Types in selium-wire
`selium-wire` SHALL provide `RpcClient<Req, Rep, M>`, `RpcConnection<Req, Rep, M>`, `RpcRequest<Req, Rep, M>`, and `RpcAccept<Req, Rep>` types. These types SHALL be built on `FramedRead<M>`/`FramedWrite<M>` over `MessageTransport`.

#### Scenario: RpcClient is importable from selium-wire
- **WHEN** a crate imports `selium_wire::rpc::RpcClient`
- **THEN** the import SHALL resolve to the canonical `RpcClient` type

### Requirement: Guest log transport module
`selium-guest` SHALL provide a `log` module behind `feature = "logging"` containing `init()` and `init_with_capacity()`. The module SHALL use `selium-shm` channels (Drop backpressure) for log transport.

#### Scenario: Guest initialises logging
- **WHEN** a guest calls `selium_guest::log::init()`
- **THEN** a tracing subscriber SHALL be installed and `tracing::info!(...)` SHALL publish to the log channel

### Requirement: Log record types re-exported
`selium-guest::log` SHALL re-export `LogRecord`, `LogField`, `LogSpan`, and `LogLevel` from `selium-encoding`.

#### Scenario: Log record fields are accessible
- **WHEN** a subscriber decodes a `LogRecord`
- **THEN** the `level`, `target`, `message`, `fields`, `spans`, and `timestamp_ms` fields SHALL be readable

### Requirement: ResourceKind threading in RingBuf
`RingBuf::create` (in `selium-shm`) SHALL accept a `ResourceKind` parameter and thread it through to the `RegionProvider::allocate` call as the `purpose` field.

#### Scenario: RingBuf created with LogChannel purpose
- **WHEN** `RingBuf::create(capacity, ResourceKind::LogChannel)` is called
- **THEN** the `RegionProvider::allocate` call SHALL carry `purpose: ResourceKind::LogChannel`
