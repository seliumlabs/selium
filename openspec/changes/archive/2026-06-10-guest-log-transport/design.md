## Context

**Current state:**
- `GuestLog::write(entry)` sends `GuestLogEntry` via `HostcallRequest::GuestLogWrite` — a synchronous, per-entry hostcall that pushes into `kernel::process::Process::guest_logs: Mutex<Vec<GuestLogEntry>>`.
- `GuestLog::read_from(cursor, process_id)` issues `HostcallRequest::GuestLogRead` and returns entries from the kernel-side vec.
- There is no tracing subscriber wired up; `tracing` macros are re-exported but events go nowhere.
- `Context` has `lookup(uri)` for discovery resolution, but no `register`/`revoke` for publishing URI→resource mappings.
- `DiscoveryRequest` only has `Resolve(String)`; the discovery guest's `DiscoveryStore` has `register`/`remove` methods but they are only reachable via its `#[pattern_interface]` trait (not yet wired to the RPC handler).
- `Channel::create(capacity)` has no backpressure parameter. Backpressure is implicit: `Writer` is non-blocking, `BlockingWriter` respects reader positions.
- The runtime dispatches `AllocRegion` hostcalls but does not track or publish what the region is used for.
- The prior art in `main/system/userland/src/logging.rs` demonstrates a complete pub/sub log transport with `Channel::create_with_backpressure`, `SharedChannel`, `LogUriRegistrar`, `tracing_subscriber::Layer`, and FlatBuffer-encoded `LogRecord`.

**Constraints:**
- WASM guests cannot block the runtime; all I/O must be async.
- A malicious guest controls its own WASM linear memory and can craft arbitrary hostcall payloads. The guest library is a convenience, not a security boundary. Only the runtime's hostcall dispatch is authoritative.
- The `#[schema]` proc macro already generates FlatBuffers encode/decode from `.fbs` files — we reuse it for log records.
- The discovery guest is already an RPC server handling `DiscoveryRequest::Resolve`; adding `Register`/`Revoke` is a minimal extension.
- The kernel must be a log subscriber to retain the existing `read_from` capability for non-guest consumers (e.g., host-side tooling).
- The runtime already tracks every allocated region and queue per-process. Adding a resource-kind tag and discovery publication is incremental.

## Goals / Non-Goals

**Goals:**
- Transport guest log records over a shared-memory pub/sub channel, not per-entry hostcalls.
- Allow authorised third-party guests to subscribe to log streams via URI discovery.
- Prevent log consumers from blocking the publishing guest (Drop backpressure).
- Integrate with `tracing` so `tracing::info!()` and friends automatically publish to the log channel.
- Provide authoritative, runtime-enforced resource registration that guests cannot bypass.
- Provide `Context::register` and `Context::revoke` for guest-requested custom URIs, validated against runtime-published ownership.
- Kernel remains a subscriber to preserve host-side log access.

**Non-Goals:**
- Log persistence or durability (that's `DurableLog`'s domain).
- Log filtering, sampling, or level-based routing at the channel level (consumers do that downstream).
- Replacing `ActivityLog` or `Metering` — those are separate observability surfaces.
- Multi-tenant log isolation at the transport layer (that's a URI-namespacing concern).
- Using `ResourceKind` for access control decisions — it is informational metadata only.

## Decisions

### 1. Channel-level `ChannelBackpressure` enum

**Decision:** Add `ChannelBackpressure` with `Park` (default) and `Drop` variants to `Channel::create`. On `Drop` channels, `blocking_writer()` returns `Error::BackpressureNotSupported`. The non-blocking `writer()` works on both.

**Rationale:** Makes the intent explicit at creation time — "this channel is fire-and-forget." Prevents accidentally using a blocking writer on a log channel and creating unintended backpressure. Mirrors the prior art's `ChannelBackpressure::Drop`.

**Alternative considered:** Keep backpressure implicit via writer type selection (current arch3 design). Rejected because it's too easy to call `blocking_writer()` on a log channel and silently introduce blocking behaviour.

### 2. FlatBuffers for log record wire format

**Decision:** Define `logging.fbs` with `LogRecord`, `LogField`, `LogSpan`, `LogLevel` tables. Use the `#[schema]` proc macro to generate Rust types and FlatBuffer encode/decode.

**Rationale:** The `#[schema]` macro already exists and is used for discovery messages. FlatBuffers provide zero-copy reads for consumers that don't need every field. Schema evolution is built-in. Matches the prior art's wire format exactly.

**Alternative considered:** Use `rkyv` (already used for hostcall payloads). Rejected because `rkyv` requires the full type to deserialize; FlatBuffers allow partial reads. Additionally the schema macro tooling is already integrated with `.fbs` files and `flatc-fork`.

### 3. Tracing subscriber Layer pattern

**Decision:** `LogLayer` implements `tracing_subscriber::Layer`. `on_event` builds a `LogRecord` via `FlatBufferBuilder`, publishes it onto a `Writer` (non-blocking). Use a `ForwardingGuard` (thread-local `Cell<bool>`) to suppress re-entrant events. Global state in `OnceLock<LoggingState>`.

**Rationale:** Proven pattern from prior art. The re-entrancy guard is essential because the writer's `send` may itself trigger log events (e.g., async runtime diagnostics).

**Alternative considered:** A dedicated background task that reads from a `tracing_channel` and writes to the I/O channel. Rejected because it adds latency, a spawn point, and buffering complexity for no benefit over the direct Layer approach with a guard.

### 4. Two-tier resource registration

**Decision:** Resource registration is split into two tiers:

**Tier 1 — Runtime-authoritative (automatic, unbypassable):**
The runtime holds an `RpcClient<DiscoveryRequest, DiscoveryResponse>` connected to the discovery guest. On every `AllocRegion` dispatch, it sends `DiscoveryRequest::Register` for `sel://process/<process_id>/regions/<region_id>` and, when the `purpose` field provides a known alias, also for `sel://process/<process_id>/<purpose-alias>` (e.g., `sel://process/42/logs`). On process termination, the runtime revokes every process-prefixed URI.

**Tier 2 — Guest-requested (validated, custom URIs):**
`Context::register(uri, target)` and `Context::revoke(uri)` delegate to the guest's discovery RPC session. The discovery guest checks: does the `client_process_id` (set by the runtime on the RPC connection, not spoofable by the guest) own `target.resource_id` according to the Tier-1 ownership table? If yes, the custom URI is registered. If no, `DiscoveryResponse::Forbidden` is returned.

**Rationale:** The runtime is the only place that sees every resource allocation and can authoritatively attest ownership. Guests cannot spoof Tier-1 registrations because they don't control the runtime's RPC session. Tier-2 allows semantic naming (e.g., `sel://my-app/production-logs`) without sacrificing authority — the discovery guest validates against the runtime's ownership table.

**Alternative considered:** Guest-only registration via `Context::register` without runtime involvement. Rejected because a malicious guest could register arbitrary URIs pointing to resources it doesn't own, or could simply not register at all (no discovery). Runtime enforcement is the only reliable approach.

**Alternative considered:** Hostcall-based registration (`HostcallRequest::PublishResource`). Rejected because the runtime already sees every allocation — adding a separate hostcall would be redundant and create a consistency problem (guest could allocate without registering).

### 5. Explicit `ResourceKind` on `AllocRegion`

**Decision:** Add a `ResourceKind` enum with variants `LogChannel`, `LiveTable`, `RpcRing`, `PubSubTopic`, `NetworkBuffer`, `DurableLog`, `BlobStore`, and `SharedMemory`. The `AllocRegion` hostcall gains a non-optional `purpose: ResourceKind` field. `RingBuf::create` threads it through from the caller. The runtime records the purpose and uses it for discovery aliases but does **not** use it for AAA — a guest can lie about purpose, but the only effect is a misleading entry in a UI or audit log.

**Rationale:** Gives the runtime enough information to generate purpose-specific discovery URIs (e.g., `sel://process/42/logs` vs `sel://process/42/tables/state`) without requiring the guest to make a separate registration call. The non-AAA stance keeps the field simple — no validation burden, no attack surface.

**Alternative considered:** Heuristic detection (e.g., inspect the region's contents to guess purpose). Rejected as fragile and racy — the runtime would need to read guest memory at the right moment.

**Alternative considered:** Optional `purpose` with a separate registration hostcall for the alias. Rejected as it reintroduces the consistency problem — the guest could forget to register, and the log channel would only appear under the opaque `sel://process/<id>/regions/<n>` URI.

### 6. Kernel as log channel subscriber

**Decision:** During `selium_guest::log::init()`, the guest creates a Drop-backpressure channel, shares it, and sends the shared region id to the kernel via a new `HostcallRequest::GuestLogRegister { shared_id }`. The kernel attaches to the shared region and creates a `Reader` (non-blocking). The kernel's `read_guest_logs_from` reads from this reader's buffer, converting `IoFrame`s back into `GuestLogEntry` structs for host-side consumers.

**Rationale:** Removes the per-entry hostcall bottleneck while preserving backward compatibility for host-side log access. The kernel reads at its own pace; the guest never blocks on kernel consumption (Drop backpressure). The runtime validates the `GuestLogRegister` hostcall — the `shared_id` must belong to the calling process.

**Alternative considered:** Keep `GuestLogWrite` hostcall but also publish to channel (dual-write). Rejected as wasteful — every entry would still hit the hostcall path.

### 7. Feature gating

**Decision:** The `log` module is behind `feature = "logging"`, default on. `tracing-subscriber` is an optional dependency gated on this feature.

**Rationale:** Guests that don't need structured log transport (e.g., minimal system guests) can opt out and avoid pulling in `tracing-subscriber`.

## Risks / Trade-offs

- **Data loss on slow consumers**: Drop backpressure means a consumer that falls behind loses log records (`Error::Overwritten`). → Mitigation: consumers can upgrade to `BlockingReader` which the publisher ignores (Drop semantics), but they risk falling further behind. The kernel reader is non-blocking and may also lose records under extreme load. This is acceptable for operational logs; durable audit logs should use `DurableLog`.
- **Channel capacity sizing**: 512KB (matching prior art) may be too small for verbose guests or too large for memory-constrained ones. → Mitigation: `init_with_capacity()` allows callers to override.
- **FlatBuffer builder allocation per event**: Each log event allocates a `FlatBufferBuilder` and `Vec<u8>` for the encoded record. → Acceptable trade-off; the prior art does the same. Allocation pressure is bounded by the channel's backpressure (Drop discards, doesn't queue).
- **Global OnceLock init**: Only one log channel per guest. Guests needing multiple log streams (e.g., per-tenant) must create additional channels manually. → This matches the prior art; multi-stream logging is deferred.
- **Spoofed `ResourceKind`**: A malicious guest can set `purpose: LogChannel` on a non-log allocation, creating a misleading discovery entry. → The entry is still scoped under `sel://process/<id>/` and only resolves within the tenant. A consumer that subscribes to the spoofed URI gets whatever bytes the guest writes — the same as if the guest used a custom URI. The UI impact is cosmetic.
- **Runtime holding discovery RPC session**: The runtime must connect to the discovery guest during bootstrap (before any process starts). If discovery is unavailable, resource registration fails but process execution can proceed with degraded discoverability. → The runtime logs a warning and retries on a backoff. The session is re-established on discovery guest restart.

## Migration Plan

1. Add `ResourceKind` enum and `ChannelBackpressure` enum to `selium-abi`.
2. Add `purpose: ResourceKind` to `AllocRegion` hostcall. Update all `RingBuf::create` call sites to pass the appropriate kind (existing callers pass `SharedMemory`).
3. Add `Register`/`Revoke`/`Forbidden` to `DiscoveryRequest`/`DiscoveryResponse`. Update discovery guest handler.
4. Wire runtime to discovery guest: add RPC session during bootstrap, publish every `AllocRegion`, revoke on termination.
5. Update `Channel::create` signature to require `ChannelBackpressure`. Update all callers.
6. Add `Context::register`/`Context::revoke` (Tier 2).
7. Add `logging.fbs` schema, run `flatc-fork` build, add generated code.
8. Implement `selium_guest::log` module with `init()`/`init_with_capacity()`.
9. Add `GuestLogRegister` hostcall, update kernel to subscribe to log channels.
10. Deprecate `GuestLog::write` and `GuestLog::read_from` with `#[deprecated]` attributes.
11. Once all system guests are migrated, remove the deprecated hostcalls.

**Rollback:** The `GuestLogWrite`/`GuestLogRead` hostcalls are retained (deprecated, not removed). Guests compiled against the old API continue to work. The kernel continues to support both the old `guest_logs: Vec` path and the new channel path during the transition.

## Open Questions

1. Should the kernel log reader be blocking or non-blocking? Non-blocking risks data loss; blocking risks backpressure on the kernel. Leaning non-blocking for operational logs.
2. Should the runtime publish a structured resource manifest at `sel://process/<id>` (listing all regions by kind), or only individual URIs? Individual URIs are sufficient for lookup; a manifest is a nice-to-have for UI inspection. Deferred to follow-up.
3. Should `ResourceKind` include capability-gating? Currently no — a process with `SharedMemory` capability can allocate any kind. The kind is purely informational. If a future change adds per-kind capabilities, the runtime already has the field to validate against.
