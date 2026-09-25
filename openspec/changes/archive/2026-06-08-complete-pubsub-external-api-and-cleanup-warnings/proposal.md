## Why

The build currently produces four categories of warnings beyond the known `selium-discovery` warnings. These are not leftover dead code — they are stubs for genuine future work that has no spec coverage yet:

1. **Pub/sub generation-change detection** (`pubsub.rs` — `last_generation`): The `Subscriber<T>` stores a `last_generation` field to detect when the publisher's generation counter has wrapped past the subscriber's read position (indicating overwritten data), but the detection logic was never implemented.

2. **Non-blocking channel poll** (`reader.rs` — `has_ready_frame`): `StrongReader` has a `has_ready_frame` method for non-blocking readiness checks, built for async polling integrations (Quinn), but it's never called because the Quinn integration is blocked on other work.

3. **External API crate** (`external-api/src/lib.rs` — six unused functions): The `selium-external-api` guest crate implements a complete text-protocol parser that accepts user requests (`"deploy <id> <replicas>"`, `"start ..."`, `"scale ..."`) and decomposes them into `DelegatedInteraction` steps dispatched to discovery and scheduler. The entrypoint is a placeholder: *"external API transport is blocked until the runtime exposes a configured inbound network bridge"*. The entire crate has no spec.

4. **Kernel mapping state fields** (`kernel/src/state.rs` — `page_offset`, `prot`, `reader_slot`): `SharedMappingState` stores these fields but never reads them. The `per-page-memory-protection` spec covers enforcement via wasmtiny's `mprotect`, making these kernel-side fields vestigial.

## What Changes

- **Implement pub/sub generation-change detection** in `Subscriber<T>`: when the publisher bumps the generation counter past the subscriber's tracked value by more than the ring capacity allows, detect overwrites and surface `Error::Overwritten`
- **Wire `has_ready_frame` into a public async poll path** so Quinn and other async consumers can check channel readiness without blocking
- **Spec and implement the external API**: define the text-protocol grammar, the delegation pipeline (parse → decompose → dispatch), and the inbound network bridge that connects external clients to the API guest
- **Remove vestigial fields** from `SharedMappingState` (`page_offset`, `prot`, `reader_slot`) since `per-page-memory-protection` enforces these at the wasmtiny level — or document them as intentional audit state if they serve a debug purpose

## Capabilities

### New Capabilities
- `external-api`: A text-protocol gateway guest that accepts user requests, parses them into `UserIntent`, decomposes intents into `DelegatedInteraction` steps, and dispatches to discovery/scheduler over RPC — exposed to external clients via a runtime-managed inbound network bridge

### Modified Capabilities
- `selium-guest`: `Subscriber<T>` gains generation-change detection and `Error::Overwritten` variant; `StrongReader::has_ready_frame` wired into the public async API; `Error` enum gains `Overwritten` variant
- `selium-kernel`: `SharedMappingState` struct shrinks to only fields that are actively read; vestigial fields removed unless needed for upcoming network proxy work

## Impact

| Area | Impact |
|------|--------|
| `selium-guest` | `pubsub.rs`: new generation-wraparound check in `Subscriber::recv`; `reader.rs`: `has_ready_frame` exposed via `poll_ready`; `error.rs`: new `Overwritten` variant |
| `selium-kernel` | `state.rs`: `SharedMappingState` loses `page_offset`, `prot`, `reader_slot` (or gains documentation if kept) |
| `selium-external-api` | Graduate from stub crate to spec'd component: text-protocol parser, delegation dispatch, RPC client wiring to discovery/scheduler |
| `selium-runtime` | New inbound network bridge configuration passed during bootstrap; external API guest receives bridge handle |
| `crates/guests/scheduler` | Must implement RPC service for `SchedulerPlace`/`SchedulerStop`/`SchedulerScale` interactions |
