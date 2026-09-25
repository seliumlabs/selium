## Context

The arch3 guest SDK (`selium-guest`) currently uses `rkyv` for all serialization — pub/sub messages, RPC requests/replies, and live table records. The prior art in `../main/system/userland/` demonstrates a proven two-layer approach: `rkyv` for internal hostcall (driver) transport, and Flatbuffers for public wire protocols between guests. This design extends that pattern to arch3.

The framing layer (`FrameHeader`, `FramedRead`, `FramedWrite`, ring buffer I/O) is already codec-agnostic — it operates on raw `[u8]` payloads. This means the serialization codec can be swapped without touching the transport layer.

## Goals / Non-Goals

**Goals:**
- Replace `rkyv` with Flatbuffers for all guest-side I/O codec paths (pub/sub, RPC, live tables)
- Provide `.fbs` schema files as canonical, shareable, versionable message definitions
- Provide a `#[schema]` proc macro that bridges idiomatic Rust types to generated Flatbuffers bindings with minimal boilerplate
- Enable content-addressable schemas via BLAKE3 hashing for compile-time schema change detection
- Keep `rkyv` for the internal hostcall/ABI boundary unchanged

**Non-Goals:**
- Changing the framing layer or ring buffer I/O
- Changing `selium-abi` types or the hostcall protocol
- Replacing `rkyv` on the host/runtime side
- Implementing schema negotiation at runtime (future work)
- Supporting Flatbuffers unions for generic type dispatch (opaque bytes approach is simpler and sufficient)

## Decisions

### Decision 1: Flatbuffers over Cap'n Proto, Protobuf, or MessagePack

**Chosen: Flatbuffers**

Rationale:
- **Zero-copy access**: Flatbuffers allows reading fields without parsing the entire message — critical for WASM guests where memory and CPU are constrained. Protobuf requires a full parse step.
- **Prior art**: `../main/system/userland/` already uses Flatbuffers successfully with the `#[schema]` macro pattern. Reusing this approach avoids reinventing the wheel.
- **Schema evolution**: Flatbuffers supports adding/removing fields with defaults, deprecating fields, and schema composition — all without breaking existing consumers.
- **Broad language support**: Official Flatbuffers support for Rust, C++, Go, Java, Python, TypeScript, C#, Kotlin, Swift, Dart, Lua, PHP — matching the platform's polyglot ambitions.
- **No runtime compiler needed**: `flatc` runs at build time only. At runtime only the lightweight `flatbuffers` crate is needed.

Alternatives considered:
- **Cap'n Proto**: Good zero-copy story and schema evolution, but Rust ecosystem is less mature, fewer language bindings, and no existing prior art in the project.
- **Protobuf**: Industry standard, excellent tooling, but requires a full parse step (no zero-copy), and the Rust ecosystem (`prost`, `rust-protobuf`) adds significant compile-time overhead.
- **MessagePack**: No schema support — doesn't meet the shareable/versionable requirement.
- **Stay with rkyv**: Would require all consumers to use Rust, which violates the cross-language interop goal.

### Decision 2: Two-Layer Architecture (rkyv for ABI, Flatbuffers for I/O)

The hostcall boundary (`HostcallEnvelope`, `CompletionState`, driver arguments) stays on `rkyv`. This is an internal transport layer — not a public API — and it benefits from `rkyv`'s zero-copy archive model where host and guest share WASM linear memory.

The guest I/O layer (pub/sub, RPC, live tables) switches to Flatbuffers. This is the public API surface — messages that flow between guests and potentially to external consumers.

```
┌──────────────────────────────────────────────────┐
│  Guest Code                                      │
│  ┌──────────────────────────────────────────┐    │
│  │  I/O Patterns (pubsub, rpc, tables)      │    │
│  │  Codec: Flatbuffers (FlatMsg trait)      │    │
│  │  Schema: .fbs files + #[schema] macro    │    │
│  └──────────────┬───────────────────────────┘    │
│                 │ FramedRead / FramedWrite        │
│                 │ (raw bytes, codec-agnostic)     │
│  ┌──────────────┴───────────────────────────┐    │
│  │  Hostcalls (hostcall.rs)                 │    │
│  │  Codec: rkyv (Archive/Serialize)         │    │
│  │  Types: HostcallEnvelope, CompletionState│    │
│  └──────────────┬───────────────────────────┘    │
└─────────────────┼────────────────────────────────┘
                  │ WASM linear memory
┌─────────────────┼────────────────────────────────┐
│  Host / Runtime │                                │
│  Codec: rkyv    │                                │
└─────────────────┴────────────────────────────────┘
```

### Decision 3: Schema Infrastructure Placement

**Chosen: Co-locate in `selium-guest`**

The `FlatMsg`/`HasSchema`/`FieldEncoder` traits, `SchemaDescriptor` type, `.fbs` files, and `build.rs` all live in `selium-guest`. The `#[schema]` proc macro lives in the existing `selium-guest-macros` crate.

Rationale:
- Follows the prior art pattern: `selium-userland` contains its own `encoding.rs` module with the same traits
- Keeps the codec infrastructure close to the I/O patterns that use it
- Avoids a proliferation of small crates (no `selium-codec`, `selium-codec-macros`)
- `selium-guest-macros` already exists as a proc-macro crate with `syn`/`quote` dependencies

### Decision 4: Generic Type Strategy for LiveTableMessage

Flatbuffers schemas are monomorphic — you can't define `table LiveTableMessage<K, V>`. The solution is to carry opaque bytes:

```fbs
table LiveTableMessage {
  mutation_id: ulong;
  key_bytes: [ubyte];         // K encoded via FlatMsg
  value_bytes: [ubyte];       // V encoded via FlatMsg (empty = tombstone)
  expected_version: ulong;    // 0 = none
}
```

The `LiveTable<K: FlatMsg, V: FlatMsg>` struct handles the two-level encoding:
1. **Encode**: `FlatMsg::encode(&key)` → bytes, `FlatMsg::encode(&value)` → bytes → wrap in `LiveTableMessage` → `FlatMsg::encode(&msg)` → frame
2. **Decode**: frame → `FlatMsg::decode::<LiveTableMessage>(&bytes)` → `FlatMsg::decode::<K>(&key_bytes)` + `FlatMsg::decode::<V>(&value_bytes)`

This is the same pattern used by `FlatResult` in the prior art, which carries an opaque `payload: Vec<u8>` for nested Flatbuffers messages.

For RPC and pub/sub, the generic types `Req`/`Rep`/`T` directly implement `FlatMsg` — no wrapping needed since those messages are self-contained Flatbuffers tables.

### Decision 5: Wire Type / Domain Type Pattern

Following the prior art, structs annotated with `#[schema]` are "wire types" — pure data carriers that map 1:1 to Flatbuffers tables. Domain types (like `LiveTable<K,V>`) implement `HasSchema` and `FlatMsg` manually, delegating to wire types.

```rust
// Wire type (generated by #[schema] macro)
#[derive(Clone, Debug, PartialEq)]
#[schema(
    path = "schemas/live_table.fbs",
    ty = "selium.live_table.LiveTableMessage",
    binding = "crate::fbs::selium::live_table::LiveTableMessage"
)]
struct LiveTableMessageWire {
    mutation_id: u64,
    key_bytes: Vec<u8>,
    value_bytes: Vec<u8>,
    expected_version: u64,
}

// Domain type (manual impl for generics)
impl<K: FlatMsg, V: FlatMsg> FlatMsg for LiveTableMessage<K, V> {
    fn encode(value: &Self) -> Vec<u8> { /* encode K, V as bytes, wrap */ }
    fn decode(bytes: &[u8]) -> Result<Self, InvalidFlatbuffer> { /* unwrap, decode K, V */ }
}
```

### Decision 6: Build-Time Code Generation

The `build.rs` in `selium-guest` uses `flatc-fork` (a Rust-based `flatc` binary) and `flatbuffers-build` to compile `.fbs` files into Rust source files in `src/fbs/`. This follows the exact same approach as `../main/system/userland/build.rs`.

This avoids requiring a system-installed `flatc` and ensures reproducible builds. The generated files are checked into version control (or generated at build time and placed in `OUT_DIR` — the prior art generates into `src/fbs/` for IDE friendliness).

### Decision 7: Schema Versioning

Flatbuffers provides native schema evolution:
- Adding new fields (with defaults) is backward-compatible
- Deprecating fields preserves wire compatibility
- `file_identifier` and `root_type` provide runtime type identification

The `SchemaDescriptor` adds a content-addressable layer:
- 16-byte BLAKE3 hash of the `.fbs` file content
- Changes to the schema produce a different hash at compile time
- Enables future schema negotiation: endpoints can exchange hashes to verify compatibility

## Risks / Trade-offs

- **Larger wire size**: Flatbuffers includes field offsets and vtable metadata. For small messages, this overhead is noticeable. Mitigation: Flatbuffers' zero-copy access means we don't pay deserialization CPU cost, and the schema-driven approach gives us the option to use `force_align` and other space optimizations.

- **Build dependency on flatc-fork**: `flatc-fork` is a less-commonly-used crate. Mitigation: It's already vetted in the prior art (`../main/`), and the generated code is checked in so builds work without it once the generated files exist.

- **Two codecs in one crate**: `selium-guest` will depend on both `rkyv` (transitively via `selium-abi` for hostcalls) and `flatbuffers` (for I/O). Mitigation: The code paths are clearly separated — `hostcall.rs` is the only module touching `rkyv` types directly, and the I/O modules only use `FlatMsg`.

- **Breaking change for guest code**: All guest crates must switch from `#[derive(Archive, Serialize, Deserialize)]` to `#[schema(...)]` and define `.fbs` files. Mitigation: Migration is mechanical — same number of lines, just different annotations. The `#[schema]` macro generates all the boilerplate.

## Open Questions

1. **Should generated Flatbuffers bindings live in `src/fbs/` (committed) or `OUT_DIR` (build-time only)?** The prior art uses `src/fbs/` for IDE support. We should follow that pattern but ensure the generated files are regenerated if schemas change (via `cargo:rerun-if-changed=schemas/`).

2. **Do we need a `FlatMsg` blanket impl for all `#[schema]` types?** The prior art generates `FlatMsg` impls in the `#[schema]` macro. We should do the same to avoid requiring manual impls for wire types.

3. **What schema files are needed initially?** At minimum: `live_table.fbs` for `LiveTableMessage`. Additional schemas can be added as guest crates need them — the infrastructure supports any number of `.fbs` files.
