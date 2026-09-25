## Purpose

`selium-guest` is the primary SDK crate for Selium guest Wasm modules, providing safe, ergonomic handle types over shared memory ABI primitives (ring buffers, readers, writers) and re-exporting the lower crates (`selium-wire`, `selium-shm`, `selium-encoding`, `selium-memory`) so that guest code sees a single unified API surface.

## Requirements

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

#### Scenario: Prototype-local pattern composition
- **WHEN** the current arch3 prototype uses the messaging-pattern layer in native tests or single-process guest logic
- **THEN** the SDK MAY satisfy those semantics through local in-memory composition while the host-backed inter-guest fabric remains future work

### Requirement: Pub/Sub Generation-Change Detection
`Subscriber<T, M>` SHALL detect when the publisher has overwritten unread data. For transports that support generation tracking (e.g., `ShmTransport`), detection SHALL use the generation counter delta. For transports that do not (e.g., `QuicTransport`), overwrite SHALL NOT be reported.

#### Scenario: Publisher overwrites unread data (shm)
- **WHEN** a subscriber calls `Stream::poll_next()` over shm and the underlying `Reader::read_frame` returns an overwrite error
- **THEN** the subscriber SHALL surface `Error::Overwritten` through the stream

#### Scenario: Normal publishing within capacity
- **WHEN** a subscriber calls `Stream::poll_next()` and the generation counter delta is within capacity
- **THEN** the subscriber SHALL read the next available frame normally

#### Scenario: First read after subscription
- **WHEN** a subscriber calls `Stream::poll_next()` for the first time (no prior `last_generation`)
- **THEN** the subscriber SHALL set `last_generation` to the current generation counter after a successful read

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

#### Scenario: Reader detects overwrite
- **WHEN** a caller invokes `poll_read()` and the generation counter delta (current generation minus last known generation) exceeds ring capacity
- **THEN** the method SHALL return `Poll::Ready(Err(io::Error::new(io::ErrorKind::Other, Error::Overwritten)))`

### Requirement: Strong Writer Backpressure via AsyncWrite
`Writer` SHALL implement `tokio::io::AsyncWrite`. The `poll_write` method SHALL write bytes as a framed payload to the outbound ring buffer with strong-reader backpressure (`protect_readers = true`). If the buffer is full, `poll_write` SHALL return `Poll::Pending`.

#### Scenario: Writer sends bytes
- **WHEN** a caller invokes `poll_write(buf)` on a `Writer`
- **THEN** the method SHALL reserve space in the ring buffer, write a frame containing the buffer bytes with `tag = 0`, and return `Poll::Ready(Ok(buf.len()))`

#### Scenario: Writer buffer full
- **WHEN** a caller invokes `poll_write(buf)` and the ring buffer cannot accommodate the frame without overwriting unread strong-reader data
- **THEN** the method SHALL return `Poll::Pending`

### Requirement: Reader/Writer Upgrade and Downgrade
`Reader` SHALL provide `downgrade(self) -> WeakReader` that releases the reader slot and returns a weak reader at the same position. `WeakReader` SHALL provide `upgrade(self) -> Result<Reader>` that allocates a reader slot at the current position and returns a strong reader.

`Writer` SHALL provide `downgrade(self) -> WeakWriter` that decrements the writer count (compensating for the lost `Drop` decrement) and returns a weak writer with the same writer ID. `WeakWriter` SHALL provide `upgrade(self) -> Result<Writer>` that increments the writer count and returns a strong writer with the same writer ID.

#### Scenario: Strong reader downgrades to weak
- **WHEN** a caller invokes `reader.downgrade()`
- **THEN** the reader slot SHALL be released (set to 0 in shared `reader_slots`) and a `WeakReader` at the same position SHALL be returned

#### Scenario: Weak reader upgrades to strong
- **WHEN** a caller invokes `weak_reader.upgrade()`
- **THEN** a new reader slot SHALL be allocated at the current position and a `Reader` SHALL be returned

#### Scenario: Strong writer downgrades to weak
- **WHEN** a caller invokes `writer.downgrade()`
- **THEN** the writer count SHALL be decremented and a `WeakWriter` with the same writer ID SHALL be returned

#### Scenario: Weak writer upgrades to strong
- **WHEN** a caller invokes `weak_writer.upgrade()`
- **THEN** the writer count SHALL be incremented and a `Writer` with the same writer ID SHALL be returned

### Requirement: Subscriber/Publisher Upgrade and Downgrade
`Subscriber<T, M>` SHALL support upgrade/downgrade when `M = ShmTransport` by delegating to the inner transport's strong/weak conversion.

#### Scenario: Subscriber upgrades from weak to strong (shm)
- **WHEN** a caller invokes `subscriber.upgrade()` on an shm-backed subscriber using a weak reader
- **THEN** a new `Subscriber<T, ShmTransport>` SHALL be returned with strong-reader backpressure

#### Scenario: Publisher upgrades from weak to strong (shm)
- **WHEN** a caller invokes `publisher.upgrade()` on an shm-backed publisher using a weak writer
- **THEN** a new `Publisher<T, ShmTransport>` SHALL be returned providing backpressure protection to readers

### Requirement: Safety Comments on Unsafe Impls
The `unsafe impl Send for RegionMappingInner` and `unsafe impl Sync for RegionMappingInner` blocks SHALL include safety comments explaining why the raw-pointer-bearing struct satisfies these auto-traits in both WASM and native modes.

#### Scenario: Safety comment documents Send/Sync rationale
- **WHEN** a developer reads the `unsafe impl` blocks in `region.rs`
- **THEN** they SHALL find comments explaining that in WASM mode the pointer references shared linear memory valid for the guest's lifetime, and in native mode the pointer is into an `Arc<Vec<u8>>` kept alive by `_backing`

### Requirement: LiveTable in selium-wire
`LiveTable<K, V>` SHALL reside in `selium-wire::tables` as the single canonical implementation, generic over `MessageTransport`.

#### Scenario: LiveTable is importable from selium-wire
- **WHEN** a crate imports `selium_wire::tables::LiveTable`
- **THEN** the import SHALL resolve to the canonical `LiveTable` type

#### Scenario: LiveTable is re-exported from selium-guest
- **WHEN** a guest crate imports `selium_guest::io::tables::LiveTable`
- **THEN** the import SHALL resolve to `selium_wire::tables::LiveTable` via re-export

### Requirement: RPC Types in selium-wire
`selium-wire` SHALL provide `RpcClient<Req, Rep, M>`, `RpcConnection<Req, Rep, M>`, `RpcRequest<Req, Rep, M>`, and `RpcAccept<Req, Rep>` types. These types SHALL be built on `FramedRead<M>`/`FramedWrite<M>` over `MessageTransport`.

#### Scenario: RpcClient is importable from selium-wire
- **WHEN** a crate imports `selium_wire::rpc::RpcClient`
- **THEN** the import SHALL resolve to the canonical `RpcClient` type

#### Scenario: RpcClient is re-exported from selium-guest
- **WHEN** a guest crate imports `selium_guest::io::rpc::RpcClient`
- **THEN** the import SHALL resolve to `selium_wire::rpc::RpcClient` via re-export

### Requirement: Guest log transport module
`selium-guest` SHALL provide a `log` module behind `feature = "logging"` containing `init()` and `init_with_capacity()`. The module SHALL use `selium-shm` channels (Drop backpressure) for log transport.

#### Scenario: Guest initialises logging
- **WHEN** a guest calls `selium_guest::log::init()`
- **THEN** a tracing subscriber SHALL be installed and `tracing::info!(...)` SHALL publish to the log channel

#### Scenario: Logging not initialised
- **WHEN** a guest calls `tracing::info!("hello")` before calling `log::init()`
- **THEN** the event SHALL be silently discarded (no subscriber installed)

### Requirement: Log record types re-exported
`selium-guest::log` SHALL re-export `LogRecord`, `LogField`, `LogSpan`, and `LogLevel` from `selium-encoding`.

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
`RingBuf::create` (in `selium-shm`) SHALL accept a `ResourceKind` parameter and thread it through to the `RegionProvider::allocate` call as the `purpose` field.

#### Scenario: RingBuf created with LogChannel purpose
- **WHEN** `RingBuf::create(capacity, ResourceKind::LogChannel)` is called
- **THEN** the `RegionProvider::allocate` call SHALL carry `purpose: ResourceKind::LogChannel`

### Requirement: Guest log handle deprecation
`GuestLog::write` and `GuestLog::read_from` SHALL be marked `#[deprecated]` with a note pointing users to `selium_guest::log::init()` for channel-based log transport.

#### Scenario: Deprecated GuestLog::write still functions
- **WHEN** existing code calls `GuestLog::write(entry)`
- **THEN** the entry SHALL be written via the `GuestLogWrite` hostcall as before
- **AND** the compiler SHALL emit a deprecation warning

### Requirement: Reactor Parking and Wake Sources

The guest executor SHALL run runnable tasks to completion or until they park, and independent runnable tasks SHALL be permitted to advance concurrently across the guest's worker threads. A task that parks on a channel generation counter SHALL consume no worker while parked, and SHALL be woken when its registered generation advances — via the host mailbox or an in-guest futex wait — never via self-scheduled repolling. The executor SHALL NOT spin while only parked tasks remain.

#### Scenario: Reactor stalls on channel waits

- **WHEN** all runnable tasks complete and remaining tasks wait on channel generation counters
- **THEN** `poll_reactor` returns rather than spinning, and the next generation advance re-runs the affected tasks and resumes the waiters

#### Scenario: Channel wait resumes on generation advance

- **WHEN** a parked task's registered generation counter advances
- **THEN** the task SHALL be woken and SHALL observe the new data without a polling interval elapsing

### Requirement: Host-Clock Timer Firing

`Timer` SHALL complete via a host-enqueued mailbox wake at the deadline;
the guest SHALL NOT poll the clock in a loop to detect expiry.

#### Scenario: Deadline wake delivery

- **WHEN** a `Timer` deadline passes while the guest reactor is stalled
- **THEN** the host enqueues a task wake for the sleeping task and the
  guest reactor resumes to complete the timer

### Requirement: Entrypoint Result Extraction

`selium-guest` SHALL provide a `run_entrypoint_with_result` function that
spawns a `Result`-producing future onto the cooperative reactor, polls
until the reactor stalls, and returns the task's output if it completed or
`Ok(())` when it parked without completing. The entrypoint export must
return an exit code immediately, so a parked entrypoint (a long-running
service, or a guest waiting on host-delivered events) reports "no error
observed" and stays on the reactor, driven by later guest polls; the
function SHALL NOT panic when the entrypoint parks. When the poll-owner
entrypoint task later completes, the reactor SHALL report completion
through the poll export so the host terminates the process as a normal
exit — the completed task SHALL NOT remain on the reactor as an idle no-op.

#### Scenario: Successful future returns its output
- **WHEN** `run_entrypoint_with_result` is called with a future that resolves to `Ok(())`
- **THEN** it SHALL return `Ok(())` after the reactor parks

#### Scenario: Failing future returns its error
- **WHEN** `run_entrypoint_with_result` is called with a future that resolves to `Err(e)`
- **THEN** it SHALL return `Err(e)` after the reactor parks
- **AND** the error SHALL be logged through the guest log transport when the task completes

#### Scenario: Entrypoint parks without completing
- **WHEN** the entrypoint future parks (a long-running service loop or a wait on host-delivered events) and the reactor stalls before the task completes
- **THEN** `run_entrypoint_with_result` SHALL return `Ok(())` (exit code 0)
- **AND** the task SHALL remain on the reactor, driven by later polls

#### Scenario: Late completion ends the process
- **WHEN** a previously parked entrypoint task completes during a later reactor poll
- **THEN** the reactor SHALL report completion through the poll export and the host SHALL terminate the process
- **AND** a later `Err` completion SHALL be logged through the guest log transport before the process terminates

#### Scenario: Panic in future aborts the guest
- **WHEN** `run_entrypoint_with_result` is called and a spawned task panics during reactor polling
- **THEN** the guest process SHALL abort (same behavior as `run_entrypoint_safely`)

### Requirement: JoinHandle Result Access
`selium-guest` SHALL expose a `pub(crate)` accessor on `JoinHandle<T>` that drains and returns the completed task's output.

#### Scenario: Result extracted after reactor parks
- **WHEN** `take_result()` is called on a `JoinHandle` whose task has completed
- **THEN** it SHALL return `Some(output)` with the task's final value

#### Scenario: Result extraction before completion
- **WHEN** `take_result()` is called on a `JoinHandle` whose task has not yet completed
- **THEN** it SHALL return `None`

### Requirement: Raw discovery handle accessor

`selium-guest` SHALL expose `Context::raw_handle()`, returning the raw
`u64` discovery handle the context was constructed from. System guests
that must forward the handle to child guests they spawn can therefore pass
it along without rebuilding a context or reconstructing the raw value from
other state.

#### Scenario: Guest forwards discovery handle to a spawned child

- **WHEN** a system guest (for example bridge-server) spawns a child guest whose own entrypoint requires the discovery handle
- **THEN** the parent SHALL read the handle via `Context::raw_handle()` and pass it as the child's first entrypoint argument
- **AND** the parent SHALL NOT need to retain a separate copy of the handle

### Requirement: Entrypoint Argument Reader

`selium-guest` SHALL provide a helper that reconstructs a slice of bytes
from a `(u64, u64)` pointer argument, so guest entrypoints do not repeat
unsafe slice construction. The helper is `unsafe` (the address and length
are trusted only because the runtime wrote them) and provides a UTF-8
variant for string configuration.

#### Scenario: Read pointer argument as bytes

- **WHEN** a guest receives a `(u64, u64)` pointer argument written by the runtime
- **THEN** `selium_guest::args::bytes(ptr, len)` SHALL return a byte slice over the payload

#### Scenario: Read pointer argument as UTF-8

- **WHEN** a guest receives a `(u64, u64)` pointer argument carrying a valid UTF-8 payload
- **THEN** `selium_guest::args::str(ptr, len)` SHALL return `Some(&str)` for valid UTF-8 and `None` otherwise

### Requirement: Concurrent Task Execution

`spawn` SHALL accept futures that are `Send + 'static`, so a spawned task capsule MAY be executed on, and migrated between, any of the guest's worker threads. Independent runnable tasks SHALL be permitted to run concurrently across the guest's workers rather than strictly one after another.

#### Scenario: Independent tasks advance in parallel

- **WHEN** a guest spawns two independent CPU-bound tasks and the guest has more than one worker
- **THEN** both tasks SHALL be able to progress concurrently on distinct workers

#### Scenario: Spawned future must be Send

- **WHEN** guest code calls `spawn` with a future that is not `Send`
- **THEN** the call SHALL fail to compile
