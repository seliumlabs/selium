## Purpose

Guest log transport provides a shared-memory channel-based logging subsystem for Selium guests, using a `tracing_subscriber::Layer` to forward structured `tracing` events as FlatBuffer-encoded `LogRecord` frames over a Drop-backpressure channel, enabling non-blocking log publication with kernel-side consumption and third-party discovery.

## Requirements

### Requirement: Guest log channel creation
`selium-guest` SHALL provide a `log` module that creates a shared-memory channel with Drop backpressure for transporting structured log records. The channel SHALL be created via `Channel::create_with_backpressure(capacity, ChannelBackpressure::Drop)` so that log consumers never block the publishing guest.

#### Scenario: Log channel is created with Drop backpressure
- **WHEN** a guest calls `selium_guest::log::init()`
- **THEN** a channel SHALL be created with `ChannelBackpressure::Drop`
- **AND** the channel's writer SHALL be a non-blocking `Writer` that ignores reader positions

#### Scenario: Log channel capacity is configurable
- **WHEN** a guest calls `selium_guest::log::init_with_capacity(1024 * 1024)`
- **THEN** the log channel SHALL be created with 1MB capacity

### Requirement: Log channel allocated with LogChannel purpose
When `log::init()` allocates the shared memory region for the log channel, the `AllocRegion` hostcall SHALL carry `purpose: ResourceKind::LogChannel`. This enables the runtime to automatically register the log channel under `sel://process/<id>/logs`.

#### Scenario: Runtime auto-registers log channel
- **WHEN** a guest calls `log::init()` and the runtime processes the `AllocRegion { purpose: LogChannel }` hostcall
- **THEN** the runtime SHALL send `DiscoveryRequest::Register { uri: "sel://process/<id>/logs", target }` to the discovery service

### Requirement: Tracing subscriber integration
`selium_guest::log::init()` SHALL install a `tracing_subscriber::Layer` that forwards all `tracing` events onto the log channel as FlatBuffer-encoded `LogRecord` frames. The subscriber SHALL be installed via `tracing_subscriber::registry().with(LogLayer).try_init()`.

#### Scenario: tracing::info! publishes to log channel
- **WHEN** a guest calls `tracing::info!("hello")` after `log::init()`
- **THEN** a `LogRecord` with level `Info`, target matching the callsite, message `"hello"`, and timestamp SHALL be published to the log channel

#### Scenario: tracing::error! with fields publishes structured record
- **WHEN** a guest calls `tracing::error!(user_id = 42, "auth failed")` after `log::init()`
- **THEN** the published `LogRecord` SHALL contain a field `user_id` with value `"42"` and message `"auth failed"`

#### Scenario: Subscriber init is idempotent
- **WHEN** a guest calls `log::init()` more than once
- **THEN** subsequent calls SHALL return `Ok(())` without installing a second subscriber

### Requirement: Re-entrancy guard
The `LogLayer` SHALL use a thread-local guard (`ForwardingGuard`) to suppress log events triggered while forwarding an earlier event. If a log event is emitted during `on_event` processing, it SHALL be silently dropped rather than causing infinite recursion.

#### Scenario: Re-entrant log event is suppressed
- **WHEN** the channel writer's `send` triggers a `tracing` event (e.g., from async runtime internals)
- **THEN** the re-entrant event SHALL be silently dropped without panicking or recursing

### Requirement: Third-party log stream discovery
Third-party guests SHALL be able to discover a process's log stream by resolving `sel://process/<process_id>/logs` via `Context::lookup` (if same tenant), or by resolving a custom URI registered by the log-producing guest via `Context::register`.

#### Scenario: Third-party guest subscribes via process URI
- **WHEN** a consumer guest resolves `"sel://process/42/logs"` via `Context::lookup`
- **THEN** the consumer SHALL receive a `ResourceTarget` with a `resource_id` that can be used to `Channel::attach` and `subscribe` to the log stream

#### Scenario: Third-party guest subscribes via custom URI
- **WHEN** a consumer guest resolves a custom URI (e.g., `"sel://my-app/production-logs"`) registered by the producing guest
- **THEN** the consumer SHALL receive the same `ResourceTarget` as the process URI

### Requirement: Log record wire format
Log records SHALL be encoded as FlatBuffers using the `logging.fbs` schema. The `LogRecord` table SHALL contain fields: `level` (LogLevel enum), `target` (string), `message` (string), `fields` (vector of `Field` tables each with `key` and `value` strings), `spans` (vector of `Span` tables each with `name` string and `fields` vector), and `timestamp_ms` (uint64, Unix milliseconds).

#### Scenario: Log record encodes and decodes round-trip
- **WHEN** a `LogRecord` is encoded via `FlatBufferBuilder` and decoded by a subscriber
- **THEN** all fields SHALL be preserved, including nested fields and spans

### Requirement: Kernel log channel subscription
During `log::init()`, the guest SHALL share the log channel and send the shared region id to the kernel via `HostcallRequest::GuestLogRegister`. The runtime SHALL validate that the shared region belongs to the calling process. The kernel SHALL attach to the shared region as a non-blocking reader, making log entries available to host-side consumers via the existing `read_guest_logs_from` API.

#### Scenario: Kernel receives log channel registration
- **WHEN** a guest calls `log::init()`
- **THEN** the kernel SHALL attach to the shared region and begin reading log records

#### Scenario: Host-side consumer reads logs from kernel
- **WHEN** a host-side caller invokes `process.read_guest_logs_from(0)`
- **THEN** log records published by the guest SHALL be returned as `GuestLogEntry` structs
