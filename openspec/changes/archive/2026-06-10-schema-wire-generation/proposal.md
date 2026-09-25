## Why

The current `#[schema]` macro requires a manual two-type pattern for generic messages: a wire type annotated with `#[schema]` and a separate domain type with a hand-written `FlatMsg` impl that bridges through the wire type. For `LiveTableMessage<K, V>` this is ~60 lines of boilerplate (wire struct definition + `FlatMsg` encode/decode impl + `HasSchema` impl). Every generic message type introduced in the future will duplicate this pattern. The macro already understands field types, the `.fbs` schema, and Flatbuffers encode/decode — it has all the information needed to generate this automatically.

## What Changes

- Extend `#[schema]` to accept a `wire` parameter that triggers automatic wire type generation and FlatMsg bridge impl generation
- When `#[schema]` is placed on a struct with generic type parameters, the macro auto-detects which fields need byte-serialization (any field whose type references a generic parameter) and generates the corresponding `*_bytes: Vec<u8>` wire fields
- `Option<T>` where `T` is a generic parameter is treated as optional byte-serialization (empty `Vec<u8>` = `None`)
- `Option<T>` where `T` is a concrete scalar uses the existing sentinel-value approach (0 = None)
- All other concrete fields pass through to the wire type unchanged
- The macro generates `LiveTableMessageWire` (or `{TypeName}Wire`) with its own `#[schema]` annotation, and a `FlatMsg` impl on the domain type that bridges through the wire type
- Update `LiveTableMessage` in `crates/core/guest/src/io/tables.rs` to use the new `wire` parameter, eliminating the manual `LiveTableMessageWire` definition and manual `FlatMsg`/`HasSchema` impls

## Capabilities

### New Capabilities

- `schema-wire-generation`: The `#[schema]` macro can generate a wire type and FlatMsg bridge impl automatically when a domain type with generic parameters is annotated with `#[schema(wire = WireTypeName)]`

### Modified Capabilities

- `selium-guest-macros`: The `#[schema]` macro gains a new `wire` parameter and the ability to generate an additional wire struct and bridge `FlatMsg` impl. New scenarios cover generic type detection, byte-serialization of generic fields, optional generic fields, and the generated bridge impl.

## Impact

- `crates/core/guest/macros/src/schema.rs` — main proc-macro implementation changes
- `crates/core/guest/macros/src/lib.rs` — new parameter parsing (`wire`)
- `crates/core/guest/src/io/tables.rs` — `LiveTableMessageWire` removed, `LiveTableMessage` gains `#[schema(wire = LiveTableMessageWire)]`, manual `FlatMsg`/`HasSchema` impls removed
- `crates/core/guest/src/encoding.rs` — no changes (existing wire types like `InterfaceMetadataWire` are not generic and continue to work as-is)
- Existing `.fbs` schemas — no changes required
