# Design: Service Message Encoding

## Context

Service messages are currently defined twice: as rkyv-derived types in `selium-abi` and as FlatBuffers `*Wire` structs in `selium-encoding`, bridged by hand-written `From`/`try_from` conversions. The `#[schema]` macro (`crates/guest/macros/src/schema.rs`) already emits `FlatMsg`/`HasSchema`/`FieldEncoder` on the annotated struct (direct mode), and its struct-with-wire mode auto-generates a byte vector mirror for generic domain types. Its enum support, however, only handles unit variants plus at most one tuple fallback mapped onto a FlatBuffers *enum* binding — which is why data-carrying enums were hand-flattened into `variant: ubyte` tables with hand-written conversions.

Current rkyv-on-wire call sites (the reason for this change):

- `crates/wire/src/control.rs` — `PipeControl` handshake frames.
- `crates/runtime/src/bootstrap.rs`, `process.rs`, `hostcall.rs` — Tier-1 `DiscoveryRequest` values published to the discovery feed.
- `guests/discovery/src/lib.rs` — the feed loop decoding those via `decode_rkyv`.
- `guests/control-plane/src/lib.rs` — `DesiredStateRecord` append/replay to the durable log.

## Goals / Non-Goals

**Goals**

- `selium-abi` defines nothing except the hostcall contract and the shared capability/resource vocabulary.
- `selium-service` is the single authority for service message types and their FlatBuffers codec.
- One hand-maintained type per message; no rkyv/Wire duplicate pair, no hand-written `From` plumbing.
- All service messages — external wire, RPC rings, Tier-1 feed, durable log — encode with FlatBuffers.
- Strict decode semantics preserved (unknown variant tag or missing required field fails, never coerces).

**Non-Goals**

- Changing the rkyv hostcall codec.
- FlatBuffers native `union` types.
- Per-guest schema ownership / schema-sync tooling.
- Extracting the shared vocabulary out of `selium-abi`.

## Decisions

### 1. Single authority crate: rename `selium-encoding` → `selium-service`

**Decision:** Rename the package (directory `crates/encoding` → `crates/service`) and make it own the canonical service types, the `.fbs` schemas, the generated bindings, and the `FlatMsg`/`HasSchema`/`FieldEncoder` machinery.

**Rationale:** `selium-encoding` is already the FlatBuffers authority and the `#[schema]` macro already resolves `encoding_path()` against it. Two crates (types + codec) would re-introduce the boundary blur that caused this bug; one crate keeps the type and its codec as a single authoring unit.

**Alternative considered:** Keep `selium-encoding` and add a separate `selium-service` types crate on top.
- Rejected: the type and its codec are the same authoring unit under `#[schema]`; a split just moves the duplication one level up.

### 2. FlatBuffers for all service messages; rkyv stays hostcall-only

**Decision:** Every service message encodes with FlatBuffers — external client wire, `selium-shm::rpc` sessions, the runtime→discovery Tier-1 feed, and the control-plane durable log. `selium-abi`'s rkyv helpers (`RkyvEncode`/`encode_rkyv`/`decode_rkyv`) are used only for hostcalls.

**Rationale:** The external client is a separate release unit (see proposal), and the desired-state log replays across upgrades; FlatBuffers' additive layout is designed for both. Using one codec everywhere also means one set of wire types for `DiscoveryRequest` instead of rkyv on the feed plus FlatBuffers on RPC.

**Alternative considered:** FlatBuffers on the external wire only; rkyv on in-process feeds and the log.
- Rejected: the log needs FlatBuffers' upgrade tolerance regardless, and a second codec for the feeds re-creates the dual-type problem for the most-shared messages (`DiscoveryRequest`). The feed and the log would then disagree with the RPC path on the same logical message.

### 3. One type per message; the macro generates enum codecs (keep the flattened table)

**Decision:** Records (`Deployment`, `ResolvedTarget`, `ResourceTarget`, `InterfaceMetadata`) keep the existing `#[schema]` struct direct-mode. Data-carrying enums (`ControlRequest`, `DiscoveryRequest`, …) become single `#[schema]`-annotated enums; the macro emits `FlatMsg`/`HasSchema` directly on the enum over the existing flattened `variant: ubyte` table. No companion wire struct, no hand-written conversion.

**Rationale:** The variable-bytes cost of the flattened table is negligible (absent fields are omitted), and it keeps the `.fbs` schemas and generated bindings stable on the wire.

**Alternative considered:** FlatBuffers native `union` (one table per variant, union member accessors).
- Rejected: native unions need a table per variant (8 for `DiscoveryRequest`) and still yield a generated borrow-based view; they move the wire layout rather than eliminate generated code. The flattened table plus macro codegen is the smaller change and preserves the current wire shape.

### 4. Attribute contract for data-enum codec generation

**Decision:** The macro derives the codec from the enum with a small attribute surface:

- **Tag** = declaration order from zero; `#[tag(n)]` on a variant overrides; duplicates or gaps are a compile error.
- **Field name** = table field name by default; `#[field("wire_name")]` renames; required on a single unnamed-field (tuple) variant, and available for names a FlatBuffers schema cannot express (e.g. Rust keywords).
- **`#[schema(skip)]`** on a field: omitted on encode, `Default` on decode (used by `DiscoveryRequest::Register.owner`, which the RPC wire does not carry).

**Rationale:** The enum source becomes the single authority for tag ordering, replacing three hand-kept places (the `.fbs` comment, the `variant: u8` doc, and the hard-coded `match` numbers).

**Alternative considered:** Parse the `.fbs` to derive tags/fields.
- Rejected: brittle text parsing and a second source of truth; declaration order plus explicit attributes is deterministic and locally verifiable.

### 5. Complete `discovery.fbs` before switching the feed

**Decision:** Extend `discovery.fbs` so `RevokeByOwner { process_id }`, `SeedDomain { domain, tenant }`, and `Register.owner` are representable before the feed moves to FlatBuffers, removing the current lossy "Tier-1 collapse to `Resolve` with empty uri".

**Rationale:** The feed *publishes* those variants today, so a FlatBuffers feed needs them on the wire; this also deletes the ugliest bespoke code in scope.

**Alternative considered:** Split `DiscoveryRequest` into an RPC enum and a separate Tier-1 feed type.
- Rejected: the feed and the RPC surface are the same logical message; one enum that round-trips fully is simpler than two types and their own codecs.

### 6. `selium-abi` keeps the shared vocabulary; `selium-service` depends on it

**Decision:** `Capability`, `ResourceClass`, `ResourceKind`, `ResourceIdentity`, `LocalityScope`, `ResourceSelector`, `CapabilityGrant`, and `ScopeContext` remain in `selium-abi` because hostcalls carry them (`ProcessStart` grants, `AllocRegion` purpose, scope evaluation). `selium-service` depends on `selium-abi` for these; the dependency direction flips from the current `encoding → abi`.

**Rationale:** This keeps the change contained, as agreed. The vocabulary genuinely crosses the hostcall boundary.

**Alternative considered:** Extract the vocabulary to a shared `selium-model` crate below both `abi` and `service`.
- Deferred, not rejected: cleaner end state, but out of scope here; revisit if `selium-abi` starts accumulating non-hostcall types again.

### 7. No compatibility shims

**Decision:** No `selium-encoding` re-export crate, no frozen rkyv service encodings.

**Rationale:** All consumers are in-workspace and no external release depends on the current name or wire format.

## Migration order

The order is chosen so the workspace compiles at every stage:

1. **Relocate** — rename the crate; move the service types into it with their existing dual codec intact (rkyv derives temporarily retained); fix imports. No behaviour change.
2. **Switch the codec** — complete `discovery.fbs` and `control.fbs`; move the feed and the durable log to `FlatMsg`; then remove rkyv derives from the service types (their last rkyv call site is gone).
3. **Single-type via macro** — extend the macro for data-enums; convert Scheduler → Control → Discovery enums, deleting each `*Wire` struct and conversion as it lands.
4. **`PipeControl`** — move to `selium-service` and convert to `#[schema]` FlatBuffers (uses the stage-3 macro).

## Risks / Trade-offs

- **Rename churn is wide** but compile-checked stage by stage.
- **The macro is new codegen.** The risk is under-tested edge cases; each attribute case gets a unit test, and the strict-decode behaviour is asserted before deleting the bespoke `try_from` code.
- **Enum ergonomics vs. flattened wire.** Consumers will match on the enum (unchanged ergonomics); the wire stays a flat table (unchanged bytes). The one residual difference is that optional/absent table fields become `Option`/default on decode, so decode must be strict about required fields — enforced in the macro.

## Open Questions

- Whether `#[tag(n)]` overrides are needed at all in v1, or declaration order only.
- Exact sentinel for an absent `Register.owner` on the wire (0 = `None`, matching the existing `OptionScalar` convention).
- Whether the enum macro must also emit `FieldEncoder` (so a data-enum can nest inside another message) in v1, or root-message-only is acceptable (no service enum nests another today).
