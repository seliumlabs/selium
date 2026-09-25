## Why

Guest log entries currently travel through blocking hostcalls (`GuestLogWrite`) into a kernel-side `Vec<GuestLogEntry>`. This forces all log traffic through the host/kernel, prevents real-time subscription by authorised third-party guests (e.g., an observability dashboard, a log aggregator), and couples log durability to hostcall latency. The prior art in `main/system/userland/src/logging.rs` demonstrates a proven alternative: transport logs over a guest I/O pub/sub channel with tracing subscriber integration, and expose the stream for subscription via URI discovery.

## What Changes

- **Two-tier resource registration**: The runtime automatically publishes every allocated resource to the discovery service under `sel://process/<id>/regions/<region_id>` (and purpose-specific aliases like `sel://process/<id>/logs`). This is authoritative — the guest cannot bypass it. Guests may additionally request custom URIs via `Context::register`, but the discovery guest validates that the calling process owns the resource before accepting the registration.
- **Explicit `ResourceKind`**: A new `ResourceKind` enum (`LogChannel`, `LiveTable`, `RpcRing`, `PubSubTopic`, `NetworkBuffer`, `DurableLog`, `BlobStore`, `SharedMemory`) is added to `selium-abi`. The `AllocRegion` hostcall gains a non-optional `purpose: ResourceKind` field. The field is not used for AAA — a malicious guest can spoof it, but the only effect is cosmetic (wrong icon in a UI). The runtime uses it automatically for discovery registration.
- **Discovery registration**: Add `Register` and `Revoke` variants to `DiscoveryRequest`/`DiscoveryResponse` in `selium-abi`. The runtime holds its own `RpcClient` to the discovery guest for authoritative Tier-1 registration. `Context::register` and `Context::revoke` provide Tier-2 guest-requested custom URIs, validated against the runtime-published ownership table.
- **Channel backpressure**: Add a `ChannelBackpressure` enum (`Park` / `Drop`) to channel creation. On `Drop` channels the publisher is never blocked by slow consumers — consumers that fall behind lose data (acceptable for logs). **BREAKING**: `Channel::create` gains a required `backpressure` parameter.
- **Guest log transport module**: New `selium_guest::log` module that creates a Drop-backpressure channel, wraps it in a `tracing_subscriber::Layer`, encodes records as FlatBuffers, and publishes them. Provides `init()` and `init_with_capacity()`. The runtime auto-registers the log channel under `sel://process/<id>/logs` when it sees the `AllocRegion { purpose: LogChannel }` hostcall.
- **Discovery guest update**: Handle `Register` / `Revoke` requests; maintain an ownership table populated by runtime Tier-1 registrations; reject guest Tier-2 registrations for resources the calling process does not own.
- **Kernel log channel**: Replace the per-process `guest_logs: Vec<GuestLogEntry>` with a shared-memory log channel registered during process bootstrap, so the kernel-side log sink becomes a channel subscriber rather than a grow-forever buffer.
- **FlatBuffers schema**: Add `schemas/logging.fbs` defining `LogRecord`, `LogField`, `LogSpan`, and `LogLevel` tables — matching the prior art's wire format.

## Capabilities

### New Capabilities
- `guest-log-transport`: Pub/sub channel for structured guest log records, with tracing subscriber integration. The runtime automatically registers log channels under `sel://process/<id>/logs`; third-party subscribers discover them via URI resolution.
- `discovery-registration`: Runtime-authoritative resource registration on every allocation, plus validated guest-requested custom URI registration. The discovery guest maintains an ownership table and enforces that custom URIs only alias resources the calling process owns.

### Modified Capabilities
- `guest-context`: `Context` gains `register(uri, target)` and `revoke(uri)` methods. The discovery guest validates ownership; registrations for unowned resources are rejected. **BREAKING**: existing `DiscoveryRequest::Resolve` only — adding `Register`/`Revoke` variants.
- `selium-abi`: `DiscoveryRequest` gains `Register { uri, target }` and `Revoke { uri }` variants. `DiscoveryResponse` gains `Registered`, `Revoked`, and `Forbidden` variants. New `ResourceKind` enum and `ChannelBackpressure` enum added. `AllocRegion` gains `purpose: ResourceKind` field. New `HostcallRequest::GuestLogRegister { shared_id }` variant for kernel-side log channel registration.
- `selium-guest`: New `log` module. `RingBuf::create` threads `ResourceKind` through to `AllocRegion`. `Channel::create` gains backpressure parameter. `GuestLog::write`/`GuestLog::read_from` deprecated in favour of channel-based transport.
- `framed-io`: `Channel` creation API gains backpressure semantics. `blocking_writer()` returns an error on `Drop`-backpressure channels. `RingBuf::create` accepts `ResourceKind`.
- `selium-runtime`: Runtime holds a discovery `RpcClient` and publishes every allocated resource on dispatch. On process termination, all process-prefixed URIs are revoked. Validates `GuestLogRegister` hostcall.

## Impact

- **Affected crates**: `selium-abi`, `selium-guest`, `selium-guest-macros` (schema proc macro), `selium-kernel`, `selium-discovery`, `selium-runtime`
- **New schema file**: `crates/core/guest/schemas/logging.fbs`
- **New module**: `crates/core/guest/src/log.rs`
- **Deprecation**: `GuestLog::write` and `GuestLog::read_from` — existing callers migrate to `selium_guest::log::init()` or direct channel subscription
- **Dependencies**: `tracing-subscriber` added to `selium-guest` (behind `logging` feature, default on). `selium-guest` types (`RpcClient`, `DiscoveryRequest`, `DiscoveryResponse`) become usable from `selium-runtime` (already a dependency for hostcall dispatch types via `selium-abi`).
