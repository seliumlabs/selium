## 1. ABI types — enums and hostcalls

- [x] 1.1 Add `ResourceKind` enum (`LogChannel`, `LiveTable`, `RpcRing`, `PubSubTopic`, `NetworkBuffer`, `DurableLog`, `BlobStore`, `SharedMemory`) to `selium-abi`
- [x] 1.2 Add `ChannelBackpressure` enum (`Park`, `Drop`) to `selium-abi`
- [x] 1.3 Add `Register { uri: String, target: ResourceTarget }` variant to `DiscoveryRequest`
- [x] 1.4 Add `Revoke { uri: String }` variant to `DiscoveryRequest`
- [x] 1.5 Add `Registered`, `Revoked`, and `Forbidden` variants to `DiscoveryResponse`
- [x] 1.6 Add `purpose: ResourceKind` field to `HostcallRequest::AllocRegion`
- [x] 1.7 Add `GuestLogRegister { shared_id: SharedResourceId }` variant to `HostcallRequest`
- [x] 1.8 Update `HostcallOutput` with any needed new variants (e.g., for GuestLogRegister errors)
- [x] 1.9 Add unit tests for all new ABI type serialisation/deserialisation round-trips

## 2. Channel backpressure implementation

- [x] 2.1 Add `BackpressureNotSupported` variant to `io::Error`
- [x] 2.2 Update `Channel::create` signature to `create(capacity: u64, backpressure: ChannelBackpressure) -> Result<Self>`
- [x] 2.3 Store `ChannelBackpressure` on `Channel` struct
- [x] 2.4 `blocking_writer()` returns `Err(Error::BackpressureNotSupported)` on Drop channels
- [x] 2.5 `Writer::poll_write` uses `protect_readers = false` on Drop channels
- [x] 2.6 Update all existing `Channel::create()` call sites to pass `ChannelBackpressure::Park`
- [x] 2.7 Add `Channel::create_with_backpressure` as an alias to `create` (matches prior art naming)
- [x] 2.8 Add unit tests: Drop channel writer never blocks, Park channel writer blocks, Drop channel rejects blocking_writer

## 3. ResourceKind threading through RingBuf and AllocRegion

- [x] 3.1 Add `purpose: ResourceKind` parameter to `RingBuf::create`
- [x] 3.2 Thread `purpose` through to `alloc_region` hostcall in `RingBuf::create`
- [x] 3.3 Update all `RingBuf::create` call sites: channels use appropriate kind (RPC rings → `RpcRing`, pub/sub → `PubSubTopic`, log → `LogChannel`), existing indeterminate callers use `SharedMemory`
- [x] 3.4 Update `Channel::create` to accept and thread `ResourceKind` (or derive from context — e.g., `Channel` callers already know their purpose)
- [x] 3.5 Add unit tests: verify `AllocRegion` hostcall payload carries correct purpose

## 4. Runtime — discovery RPC session and Tier-1 registration

- [x] 4.1 Add `selium-guest` (or at least its `io::rpc` types) as a dependency of `selium-runtime`
- [x] 4.2 During runtime bootstrap, create an `RpcClient<DiscoveryRequest, DiscoveryResponse>` connected to the discovery guest
- [x] 4.3 On `AllocRegion` dispatch: after successful allocation, send `DiscoveryRequest::Register` for `sel://process/<id>/regions/<region_id>`
- [x] 4.4 On `AllocRegion` dispatch: if purpose maps to a known alias (e.g., `LogChannel` → `sel://process/<id>/logs`), also register the alias
- [x] 4.5 Build a purpose→URI-alias mapping table in the runtime (initially: `LogChannel` → `logs`, `LiveTable` → `tables/<name>`, `RpcRing` → `rpc/<name>`; others deferred)
- [x] 4.6 On process termination: iterate all registered URIs for the process, send `DiscoveryRequest::Revoke` for each
- [x] 4.7 Add runtime-side tests: allocation publishes to discovery, termination revokes, purpose aliases work

## 5. Runtime — GuestLogRegister validation

- [x] 5.1 Add `GuestLogRegister` arm to runtime hostcall dispatch
- [x] 5.2 Validate that `shared_id` belongs to the calling process (check against process's allocated region table)
- [x] 5.3 Return error if `shared_id` is foreign
- [x] 5.4 Attach to shared region as non-blocking log reader on success
- [x] 5.5 Add runtime-side tests: valid registration succeeds, foreign shared_id rejected

## 6. Discovery guest — ownership table and Tier-2 validation

- [x] 6.1 Add ownership table to `DiscoveryStore`: `HashMap<(ProcessId, u64), ()>` mapping `(process_id, resource_id)` pairs
- [x] 6.2 On `DiscoveryRequest::Register` with URI prefix `sel://process/<id>/`, treat as Tier-1: store mapping AND populate ownership table entry for `(process_id, resource_id)`
- [x] 6.3 On `DiscoveryRequest::Register` without process prefix (Tier 2): check `client_process_id` owns `target.resource_id` via ownership table
- [x] 6.4 Return `DiscoveryResponse::Forbidden` if Tier-2 registration fails ownership check
- [x] 6.5 On `DiscoveryRequest::Revoke`: remove mapping AND ownership table entry if URIs match
- [x] 6.6 Add tenant-scoping: `Resolve` for `sel://process/<id>/` only returns `Found` if caller's tenant matches
- [x] 6.7 Update discovery guest `handler()` to extract `client_process_id` from RPC connection metadata (set by runtime)
- [x] 6.8 Add unit tests: Tier-1 populates ownership, Tier-2 rejected for unowned resource, cross-tenant process URI denied

## 7. Discovery registration — Context (Tier 2)

- [x] 7.1 Add `DiscoveryRequest::Register` and `DiscoveryRequest::Revoke` to `encoding.rs` wire type bridge (if needed for FlatBuffers codec)
- [x] 7.2 Add `DiscoveryResponse::Registered`, `DiscoveryResponse::Revoked`, and `DiscoveryResponse::Forbidden` to `encoding.rs` wire type bridge
- [x] 7.3 Add `Context::register(&mut self, uri: &str, target: ResourceTarget) -> Result<(), GuestError>`
- [x] 7.4 Add `Context::revoke(&mut self, uri: &str) -> Result<(), GuestError>`
- [x] 7.5 Map `DiscoveryResponse::Forbidden` to `Err(GuestError::Host("registration forbidden"))`
- [x] 7.6 Add unit tests for Context::register and Context::revoke (including Forbidden path)

## 8. Logging FlatBuffers schema

- [x] 8.1 Create `crates/core/guest/schemas/logging.fbs` with tables: `LogLevel` (enum), `Field`, `Span`, `LogRecord`
- [x] 8.2 Add `flatc-fork` build step for `logging.fbs` in `selium-guest` build.rs
- [x] 8.3 Add `#[schema]`-annotated Rust types in `selium_guest::log` module: `LogLevel`, `LogField`, `LogSpan`, `LogRecord`
- [x] 8.4 Add `From` conversions between `LogLevel` and `tracing::Level`
- [x] 8.5 Add unit tests: encode `LogRecord` via FlatBufferBuilder, decode back, verify field preservation

## 9. Guest log transport module

- [x] 9.1 Add `tracing-subscriber` as optional dependency (behind `logging` feature, default on)
- [x] 9.2 Create `crates/core/guest/src/log.rs` module with `InitError` enum
- [x] 9.3 Implement `LoggingState` struct holding `Channel`, `Mutex<Writer>`, `Mutex<Option<InitError>>`
- [x] 9.4 Implement `ForwardingGuard` with thread-local `Cell<bool>` re-entrancy protection
- [x] 9.5 Implement `LogLayer` (impl `tracing_subscriber::Layer`) with `on_event` → `forward_event`
- [x] 9.6 Implement `EventVisitor` (impl `tracing::field::Visit`) to extract message and fields
- [x] 9.7 Implement `forward_event`: collect spans, encode `LogRecord` via FlatBufferBuilder, publish via `Writer::send`
- [x] 9.8 Implement `init()` / `init_with_capacity()`: create channel with `ChannelBackpressure::Drop` and `ResourceKind::LogChannel`, share, register with kernel via `GuestLogRegister`, install subscriber
- [x] 9.9 Implement `channel() -> Option<Channel>` accessor
- [x] 9.10 Gate module behind `#[cfg(feature = "logging")]`, re-export from `lib.rs`
- [x] 9.11 Add unit tests: init creates channel with correct purpose, tracing event publishes record, re-entrant event suppressed

## 10. Kernel log channel subscription

- [x] 10.1 In kernel, on `GuestLogRegister` success path from runtime, attach to shared region via `RingBuf::attach(shared_id, capacity)`, create non-blocking `Reader`
- [x] 10.2 Store the log channel reader on `ProcessInner` alongside `guest_logs: Vec` (dual-path during transition)
- [x] 10.3 Update `Process::read_guest_logs_from` to drain frames from the channel reader, decode `LogRecord` → `GuestLogEntry`
- [x] 10.4 Handle `Error::Overwritten` gracefully (return available entries, skip lost ones)
- [x] 10.5 Add kernel-side test: guest registers log channel, publishes entries, kernel reads them back via channel reader

## 11. Deprecation and migration

- [x] 11.1 Mark `GuestLog::write` with `#[deprecated(note = "Use selium_guest::log::init() for channel-based log transport")]`
- [x] 11.2 Mark `GuestLog::read_from` with `#[deprecated(note = "Logs are now available via channel subscription; this method remains for host-side consumers")]`
- [x] 11.3 Add `#[allow(deprecated)]` at internal call sites that must still use the old hostcalls during transition
- [x] 11.4 Update all system guest crates (discovery, cluster, scheduler, supervisor, external-api) to call `selium_guest::log::init()` in their entrypoints

## 12. Integration and end-to-end

- [x] 12.1 Write integration test: guest calls `log::init()`, runtime auto-registers `sel://process/<id>/logs` in discovery
- [x] 12.2 Write integration test: guest calls `log::init()`, emits tracing events, kernel reads them via `read_guest_logs_from`
- [x] 12.3 Write integration test: third-party guest resolves `sel://process/<id>/logs` and subscribes to log channel
- [x] 12.4 Write integration test: guest registers custom URI via `Context::register`, another guest resolves it
- [x] 12.5 Write integration test: guest tries to register URI for unowned resource → `Forbidden`
- [x] 12.6 Write integration test: slow consumer gets `Overwritten`, publisher never blocks (Drop backpressure verified)
- [x] 12.7 Write integration test: process terminates, all process URIs revoked from discovery
- [x] 12.8 Write integration test: `GuestLog::write` (deprecated path) still functions alongside channel-based transport
- [x] 12.9 Write integration test: cross-tenant guest cannot resolve `sel://process/<id>/logs`
- [x] 12.10 Run full workspace test suite (`cargo test --workspace`), fix any regressions

## Definition of Done

- All `tracing` macro calls in guest code publish to a shared-memory log channel, not a per-entry hostcall
- The runtime automatically registers every allocated resource in discovery; guests cannot bypass this
- The runtime revokes all process URIs on termination
- Authorised third-party guests can discover and subscribe to log streams via `sel://process/<id>/logs` or custom URIs
- Guest custom URI registrations are validated: unowned resources are rejected with `Forbidden`
- A slow or stalled log consumer never blocks the publishing guest (Drop backpressure)
- The kernel remains a log consumer, preserving `read_guest_logs_from` for host-side access
- `ResourceKind` is informational only and not used for AAA decisions
- All existing tests pass; deprecated `GuestLog::write`/`read_from` still work
- All tasks above are checked `[x]`
