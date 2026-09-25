## ADDED Requirements

### Requirement: Guest log transport module
`selium-guest` SHALL provide a `log` module behind `feature = "logging"` (default on) containing:
- `init() -> Result<(), InitError>` — creates a Drop-backpressure log channel, installs the tracing subscriber, and registers the channel with the kernel
- `init_with_capacity(capacity: u64) -> Result<(), InitError>` — same as `init()` with a custom channel capacity
- `channel() -> Option<Channel>` — returns the log channel handle if initialised

#### Scenario: Guest initialises logging
- **WHEN** a guest calls `selium_guest::log::init()`
- **THEN** a `tracing_subscriber::Layer` SHALL be installed
- **AND** `tracing::info!("hello")` SHALL publish a `LogRecord` to the log channel
- **AND** the runtime SHALL auto-register the log channel under `sel://process/<id>/logs`

#### Scenario: Logging not initialised
- **WHEN** a guest calls `tracing::info!("hello")` before calling `log::init()`
- **THEN** the event SHALL be silently discarded (no subscriber installed)

### Requirement: Log record types
`selium-guest::log` SHALL export `LogRecord`, `LogField`, `LogSpan`, and `LogLevel` types derived from the `logging.fbs` FlatBuffers schema via the `#[schema]` proc macro.

#### Scenario: Log record fields are accessible
- **WHEN** a subscriber decodes a `LogRecord` from the channel
- **THEN** the `level`, `target`, `message`, `fields`, `spans`, and `timestamp_ms` fields SHALL be readable

### Requirement: InitError type
`selium-guest::log` SHALL define an `InitError` enum with variants:
- `Subscriber(String)` — tracing subscriber installation failed
- `Channel(String)` — log channel creation failed
- `Register(String)` — channel registration with kernel failed
- `Publisher(String)` — log publisher creation failed
- `Poisoned` — internal mutex poisoned

#### Scenario: InitError displays meaningful messages
- **WHEN** `log::init()` fails
- **THEN** the returned `InitError` SHALL implement `Display` and `Error` with a human-readable message

### Requirement: ResourceKind threading in RingBuf
`RingBuf::create` SHALL accept a `ResourceKind` parameter and thread it through to the `AllocRegion` hostcall as the `purpose` field.

#### Scenario: RingBuf created with LogChannel purpose
- **WHEN** `RingBuf::create(capacity, ResourceKind::LogChannel)` is called
- **THEN** the underlying `AllocRegion` hostcall SHALL carry `purpose: ResourceKind::LogChannel`

## MODIFIED Requirements

### Requirement: Safe Guest Handles
`selium-guest` SHALL provide safe, ergonomic handle types over the shared memory ABI primitives (`alloc_region`, `free_region`, `attach_region`) so guest code does not manipulate raw hostcall payloads directly for common operations.

Handles SHALL include `Reader` (strong byte-stream reader), `WeakReader` (weak byte-stream reader), `Writer` (strong byte-stream writer), `WeakWriter` (weak byte-stream writer), `FramedRead<R>` (frame-level reader), `FramedWrite<W>` (frame-level writer), `Subscriber<T>` (typed pub/sub subscriber), `Publisher<T>` (typed pub/sub publisher), `RpcClient<Req, Rep>` (typed RPC client), `RpcConnection<Req, Rep>` (typed RPC server), `LiveTable<K, V>` (materialised live table), and `LogRecord`/`LogField`/`LogSpan`/`LogLevel` (structured log types from `log` module).

#### Scenario: Guest opens primitive through SDK handle
- **WHEN** guest code acquires a shared memory, channel, or pub/sub resource through the SDK
- **THEN** the SDK SHALL expose a typed handle rather than requiring direct ABI framing code

### Requirement: Guest log handle deprecation
`GuestLog::write` and `GuestLog::read_from` SHALL be marked `#[deprecated]` with a note pointing users to `selium_guest::log::init()` for channel-based log transport.

#### Scenario: Deprecated GuestLog::write still functions
- **WHEN** existing code calls `GuestLog::write(entry)`
- **THEN** the entry SHALL be written via the `GuestLogWrite` hostcall as before
- **AND** the compiler SHALL emit a deprecation warning
