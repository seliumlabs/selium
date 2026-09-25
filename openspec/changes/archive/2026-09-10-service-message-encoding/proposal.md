# Proposal: Service Message Encoding

## Why

Service-level message types — `DiscoveryRequest`/`Response`, `ControlRequest`/`Response`, `SchedulerRequest`/`Response`, `Deployment`, `PipelineBinding`, `DesiredStateRecord`, `ResolvedTarget`, `DelegationStatus`, `ResourceTarget`, `InterfaceMetadata`, and `PipeControl` — currently live in `selium-abi` alongside the rkyv `encode_rkyv`/`decode_rkyv` helpers and all derive rkyv. The result is that several *service* paths encode these messages with rkyv even though they are not hostcalls: the runtime→discovery (Tier-1) feed, the control-plane's durable desired-state log, and the external client↔bridge per-stream handshake. Meanwhile `selium-shm::rpc` and `selium-client` already carry the same messages as FlatBuffers via `FlatMsg`.

`selium-abi`'s job is the host↔guest boundary only. The service message types are duplicated once as rkyv types in `selium-abi` and again as FlatBuffers wire types plus hand-written `From`/`try_from` conversions in `selium-encoding`. Every service message is therefore maintained twice with two codecs.

FlatBuffers is the right codec for these messages for two reasons that hold even though all crates share one repository:

1. **The native client is a separately-released binary.** `selium-client` is linked into external programs; the peer it speaks to (bridge-channel wasm + host deployment) is released and versioned independently. "Same repo" is not "deployed together" — that is the real arms-length boundary.
2. **The control-plane durable log is cross-upgrade state.** `DesiredStateRecord` is the store of record, rebuilt from replay on boot. FlatBuffers' vtable-slot layout is additive in both directions (missing field → default); rkyv's archive layout is coupled to the Rust struct's member order and presence, so adding a field changes what old bytes mean.

## What Changes

1. **Rename `selium-encoding` → `selium-service`.** It becomes the single authority for service message types, their `.fbs` schemas, generated bindings, and `FlatMsg`/`HasSchema` implementations.
2. **Move the service message types out of `selium-abi`.** `selium-abi` keeps the hostcall contract and the shared vocabulary those hostcalls carry (`Capability`, `ResourceClass`, `ResourceKind`, `ResourceIdentity`, `LocalityScope`, `ResourceSelector`, `CapabilityGrant`, `ScopeContext`). `selium-service` depends on `selium-abi` for that vocabulary.
3. **One hand-maintained type per message.** Record messages become single `#[schema]` structs. Data-carrying enums keep their Rust enum and derive their FlatBuffers codec from an extended `#[schema]` macro, deleting the hand-written `*Wire` mirror structs and `From`/`try_from` plumbing.
4. **Extend `selium-guest-macros::schema`** to generate `FlatMsg`/`HasSchema` directly on data-carrying enums against the flattened `variant: ubyte` table (tag order, field mapping, skip, strict decode).
5. **Complete `discovery.fbs`** so Tier-1-only operations (`Register.owner`, `RevokeByOwner`, `SeedDomain`) are representable; switch the runtime→discovery feed and the control-plane durable log from rkyv to FlatBuffers.
6. **Convert `PipeControl`** (client↔bridge handshake) from rkyv to FlatBuffers.

## Non-Goals

- Changing the hostcall ABI codec — `HostcallRequest`/`HostcallEnvelope`/`HostcallOutput`/`CompletionState` stay rkyv-encoded in `selium-abi`.
- Switching to FlatBuffers native `union` types — the flattened, variant-discriminated single table stays.
- Per-guest schema ownership or the `sel schema update` sync CLI — deferred until independently-releasable service bundles exist (see design.md).
- Extracting the shared vocabulary (`ResourceClass` et al.) out of `selium-abi` — deferred; kept in `selium-abi` for now.

## Impact

- **Crate rename** with updates to every dependent: `shm`, `wire`, `runtime`, `guest`, `client`, `proto-http`, `proto-dns`, `control-plane`, plus the macro's `CARGO_CRATE_NAME` branches and the `binding = "selium_encoding::…"` literals and the `../encoding/schemas/…` relative path in `wire/src/tables.rs`.
- **`selium-abi` shrinks.** Service types removed; `selium-guest` re-exports updated from `selium-abi` to `selium-service`.
- **`schema-wire-generation` spec extended** with the enum codec generation contract.
- **`control-plane` / `discovery-registration` specs updated** for FlatBuffers durable-log records and a FlatBuffers Tier-1 feed.
- **Tests updated**: `runtime/tests/{discovery,discovery_taxonomy,guest_log_transport}.rs` and the `process.rs` feed-drain helper decode the feed with `FlatMsg` instead of `decode_rkyv`.

## Risks

- **Wide but mechanical rename** touching many import sites; mitigated by doing the rename before any behavioural change so the workspace compiles at every stage.
- **The macro extension is the largest new code.** Mitigated by landing it incrementally (Scheduler enums first), with one unit test per attribute case, reusing the existing struct-path emission for encode/decode field mechanics.
- **Wire compatibility during transition.** No external release depends on the current rkyv service wire or the `selium-encoding` name, so the old encodings are not frozen and can be replaced outright.
