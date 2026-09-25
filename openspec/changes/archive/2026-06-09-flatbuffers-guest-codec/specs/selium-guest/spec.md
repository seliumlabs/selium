## MODIFIED Requirements

### Requirement: Messaging-Pattern Layer
`selium-guest` SHALL provide a messaging-pattern layer built above the shared memory substrate, using native WASM atomics for synchronization without signal hostcalls. The pattern layer SHALL use `FramedRead`/`FramedWrite` wrappers over byte-stream `Reader`/`Writer` types rather than reimplementing `FrameHeader` encoding/decoding in each pattern.

`Publisher<T>`, `Subscriber<T>`, `RpcClient<Req, Rep>`, `RpcConnection<Req, Rep>`, and `LiveTable<K, V>` SHALL use the `FlatMsg` trait for serialization instead of `rkyv`. The framing layer (`FrameHeader`, `FramedRead`, `FramedWrite`) remains codec-agnostic and unchanged.

#### Scenario: Guest selects messaging pattern
- **WHEN** guest code needs pub/sub, fanout, request/reply, stream, or live-table semantics
- **THEN** the SDK SHALL provide those semantics through the pattern layer rather than through guest-specific boilerplate

#### Scenario: Prototype-local pattern composition
- **WHEN** the current arch3 prototype uses the messaging-pattern layer in native tests or single-process guest logic
- **THEN** the SDK MAY satisfy those semantics through local in-memory composition while the host-backed inter-guest fabric remains future work

#### Scenario: Publisher encodes via FlatMsg
- **WHEN** `Publisher<T>::publish(&item)` is called
- **THEN** the item SHALL be encoded via `FlatMsg::encode` and the resulting bytes SHALL be written as a frame to the underlying ring buffer

#### Scenario: Subscriber decodes via FlatMsg
- **WHEN** `Subscriber<T>::read_with_writer_id()` reads a frame
- **THEN** the frame payload SHALL be decoded via `FlatMsg::decode::<T>`

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

### Requirement: LiveTable in selium-guest
`LiveTable<K, V>` SHALL reside in `selium-guest::io::tables` as the single canonical implementation. `LiveTableMessage<K, V>` SHALL be a Flatbuffers-backed message carrying `mutation_id: u64`, `key_bytes: Vec<u8>` (K encoded via `FlatMsg`), `value_bytes: Vec<u8>` (V encoded via `FlatMsg`, empty for tombstones), and `expected_version: u64` (0 for none). Both `K` and `V` SHALL implement `FlatMsg`.

The `LiveTable<K, V>` struct SHALL handle two-level encoding:
1. Encode K and V to bytes via `FlatMsg`, wrap in `LiveTableMessage`, encode via `FlatMsg`
2. Decode `LiveTableMessage` via `FlatMsg`, then decode K and V from the byte fields

#### Scenario: LiveTable is importable from selium-guest
- **WHEN** a guest crate imports `selium_guest::io::tables::LiveTable`
- **THEN** the import SHALL resolve to the canonical `LiveTable` type

#### Scenario: LiveTable set encodes via FlatMsg
- **WHEN** `LiveTable::set(key, value)` is called
- **THEN** the key and value SHALL each be encoded via `FlatMsg::encode`, wrapped in a `LiveTableMessage` with `value_bytes` set to the encoded V bytes, and published to the topic

#### Scenario: LiveTable sync decodes via FlatMsg
- **WHEN** `LiveTable::sync()` drains the subscriber
- **THEN** each frame SHALL be decoded as `LiveTableMessage` via `FlatMsg::decode`, and the key and value bytes SHALL be decoded via `FlatMsg::decode::<K>` and `FlatMsg::decode::<V>` respectively

## REMOVED Requirements

### Requirement: LiveTableMessage rkyv derives
**Reason**: `LiveTableMessage<K, V>` no longer uses `rkyv` for serialization. It is now a Flatbuffers-backed message with `FlatMsg` encoding.
**Migration**: Replace `#[derive(Archive, Serialize, Deserialize)]` with `#[schema(...)]` on the wire type, and implement `FlatMsg` manually on `LiveTableMessage<K, V>` to handle the generic K/V byte encoding.
