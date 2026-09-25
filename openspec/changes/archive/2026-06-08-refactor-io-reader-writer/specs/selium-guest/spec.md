## MODIFIED Requirements

### Requirement: Safe Guest Handles
`selium-guest` SHALL provide safe, ergonomic handle types over the shared memory ABI primitives (`alloc_region`, `free_region`, `attach_region`) so guest code does not manipulate raw hostcall payloads directly for common operations.

Handles SHALL include `Reader` (strong byte-stream reader), `WeakReader` (weak byte-stream reader), `Writer` (strong byte-stream writer), `WeakWriter` (weak byte-stream writer), `FramedRead<R>` (frame-level reader), `FramedWrite<W>` (frame-level writer), `Subscriber<T>` (typed pub/sub subscriber), `Publisher<T>` (typed pub/sub publisher), `RpcClient<Req, Rep>` (typed RPC client), `RpcConnection<Req, Rep>` (typed RPC server), and `LiveTable<K, V>` (materialised live table).

#### Scenario: Guest opens primitive through SDK handle
- **WHEN** guest code acquires a shared memory, channel, or pub/sub resource through the SDK
- **THEN** the SDK SHALL expose a typed handle rather than requiring direct ABI framing code

### Requirement: Messaging-Pattern Layer
`selium-guest` SHALL provide a messaging-pattern layer built above the shared memory substrate, using native WASM atomics for synchronization without signal hostcalls. The pattern layer SHALL use `FramedRead`/`FramedWrite` wrappers over byte-stream `Reader`/`Writer` types rather than reimplementing `FrameHeader` encoding/decoding in each pattern.

#### Scenario: Guest selects messaging pattern
- **WHEN** guest code needs pub/sub, fanout, request/reply, stream, or live-table semantics
- **THEN** the SDK SHALL provide those semantics through the pattern layer rather than through guest-specific boilerplate

#### Scenario: Prototype-local pattern composition
- **WHEN** the current arch3 prototype uses the messaging-pattern layer in native tests or single-process guest logic
- **THEN** the SDK MAY satisfy those semantics through local in-memory composition while the host-backed inter-guest fabric remains future work

### Requirement: Pub/Sub Generation-Change Detection
`Subscriber<T, R>` SHALL detect when the publisher's generation counter has advanced past the subscriber's last-read position by more than the ring buffer capacity, indicating that unread data has been overwritten. Detection SHALL delegate to the underlying `Reader`'s `read_frame` or `poll_read` method, which returns `Error::Overwritten` (or `io::Error` with `ErrorKind::Other` containing `Error::Overwritten` for `poll_read`).

#### Scenario: Publisher overwrites unread data
- **WHEN** a subscriber calls `Stream::poll_next()` and the underlying `Reader::read_frame` returns an overwrite error
- **THEN** the subscriber SHALL surface `Error::Overwritten` through the stream

#### Scenario: Normal publishing within capacity
- **WHEN** a subscriber calls `Stream::poll_next()` and the generation counter delta is less than or equal to the ring buffer capacity
- **THEN** the subscriber SHALL read the next available frame normally without returning `Error::Overwritten`

#### Scenario: First read after subscription
- **WHEN** a subscriber calls `Stream::poll_next()` for the first time (no prior `last_generation`)
- **THEN** the subscriber SHALL set `last_generation` to the current generation counter after a successful read

### Requirement: Non-Blocking Reader Poll
`Reader` and `WeakReader` SHALL implement `tokio::io::AsyncRead`. The `poll_read` method SHALL check whether a frame is immediately readable without blocking, and SHALL return `Poll::Ready(Ok(()))` with bytes copied to the provided buffer when data is available, or `Poll::Pending` when the ring is empty and writers are still connected.

#### Scenario: Frame is ready
- **WHEN** a caller invokes `poll_read()` and a frame with the READY flag set is at the current read position
- **THEN** the method SHALL copy frame payload bytes to the caller's buffer and return `Poll::Ready(Ok(()))`

#### Scenario: No frame ready, writers connected
- **WHEN** a caller invokes `poll_read()` and no frame is available but writer_count > 0
- **THEN** the method SHALL return `Poll::Pending`

#### Scenario: No frame ready, all writers disconnected
- **WHEN** a caller invokes `poll_read()` and no frame is available and writer_count == 0
- **THEN** the method SHALL return `Poll::Ready(Ok(()))` with zero bytes copied (EOF)

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
`Subscriber<T, R>` SHALL provide `upgrade(self) -> Result<Subscriber<T, Reader>>` when `R = WeakReader` and `downgrade(self) -> Subscriber<T, WeakReader>` when `R = Reader`. These methods upgrade or downgrade the underlying `FramedRead`'s inner reader between strong and weak variants. The return type changes to reflect the new backing handle at compile time.

`Publisher<T, W>` SHALL provide `upgrade(self) -> Result<Publisher<T, Writer>>` when `W = WeakWriter` and `downgrade(self) -> Publisher<T, WeakWriter>` when `W = Writer`. These methods upgrade or downgrade the underlying `FramedWrite`'s inner writer between strong and weak variants.

#### Scenario: Subscriber upgrades from weak to strong
- **WHEN** a caller invokes `subscriber.upgrade()` on a subscriber backed by a weak reader
- **THEN** a new `Subscriber<T, Reader>` SHALL be returned whose inner `FramedRead` wraps a strong `Reader`, providing backpressure protection

#### Scenario: Publisher upgrades from weak to strong
- **WHEN** a caller invokes `publisher.upgrade()` on a publisher backed by a weak writer
- **THEN** a new `Publisher<T, Writer>` SHALL be returned whose inner `FramedWrite` wraps a strong `Writer`, providing backpressure protection to readers

### Requirement: Safety Comments on Unsafe Impls
The `unsafe impl Send for RegionMappingInner` and `unsafe impl Sync for RegionMappingInner` blocks SHALL include safety comments explaining why the raw-pointer-bearing struct satisfies these auto-traits in both WASM and native modes.

#### Scenario: Safety comment documents Send/Sync rationale
- **WHEN** a developer reads the `unsafe impl` blocks in `region.rs`
- **THEN** they SHALL find comments explaining that in WASM mode the pointer references shared linear memory valid for the guest's lifetime, and in native mode the pointer is into an `Arc<Vec<u8>>` kept alive by `_backing`

### Requirement: LiveTable in selium-guest
`LiveTable<K, V>` SHALL reside in `selium-guest::io::tables` as the single canonical implementation. `LiveTableMessage<K, V>` SHALL derive `rkyv::Archive`, `rkyv::Serialize`, and `rkyv::Deserialize` with `#[rkyv(bytecheck())]`.

#### Scenario: LiveTable is importable from selium-guest
- **WHEN** a guest crate imports `selium_guest::io::tables::LiveTable`
- **THEN** the import SHALL resolve to the canonical `LiveTable` type

## REMOVED Requirements

### Requirement: Guest Context with Discovery RPC
**Reason**: The inline RPC implementation in `Context` is replaced by direct use of `RpcClient<DiscoveryRequest, DiscoveryResponse>` from `selium-guest::io::rpc`. The requirement for `Context` to resolve URIs is retained but specified in the `guest-context` capability.
**Migration**: `Context::lookup` now delegates to `RpcClient::request`. Guest code calling `Context::lookup` requires no changes.

## ADDED Requirements

### Requirement: RPC Types in selium-guest
`selium-guest` SHALL provide `RpcClient<Req, Rep>`, `RpcConnection<Req, Rep>`, `RpcRequest<Req, Rep>`, and `RpcAccept<Req, Rep>` types in the `io::rpc` module. These types SHALL be built on `FramedRead`/`FramedWrite` rather than working with `RingBuf` directly. The `selium-rpc` crate SHALL be removed.

#### Scenario: RpcClient is importable from selium-guest
- **WHEN** a guest crate imports `selium_guest::io::rpc::RpcClient`
- **THEN** the import SHALL resolve to the canonical `RpcClient` type

#### Scenario: RpcClient uses FramedRead/FramedWrite
- **WHEN** an `RpcClient` sends a request and receives a reply
- **THEN** it SHALL use `FramedWrite` to write the request frame and `FramedRead` to read the reply frame, rather than manipulating `FrameHeader` and `RingBuf` directly
