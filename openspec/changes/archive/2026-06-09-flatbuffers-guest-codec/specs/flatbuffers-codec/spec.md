## ADDED Requirements

### Requirement: FlatMsg Trait
`selium-guest::encoding` SHALL provide a `FlatMsg` trait that defines the encode/decode contract for Flatbuffers-backed messages transmitted over guest I/O endpoints.

```rust
pub trait FlatMsg: Sized {
    fn encode(value: &Self) -> Vec<u8>;
    fn decode(bytes: &[u8]) -> Result<Self, flatbuffers::InvalidFlatbuffer>;
}
```

#### Scenario: Round-trip encode/decode
- **WHEN** a type implementing `FlatMsg` is encoded via `FlatMsg::encode` and the resulting bytes are decoded via `FlatMsg::decode`
- **THEN** the decoded value SHALL equal the original value

### Requirement: HasSchema Trait
`selium-guest::encoding` SHALL provide a `HasSchema` trait linking a Rust type to its Flatbuffers schema descriptor.

```rust
pub trait HasSchema {
    const SCHEMA: SchemaDescriptor;
}
```

#### Scenario: Type links to its schema
- **WHEN** a developer inspects `MyType::SCHEMA`
- **THEN** it SHALL return a `SchemaDescriptor` with the fully-qualified Flatbuffers type name and the 16-byte BLAKE3 hash of the corresponding `.fbs` file

### Requirement: FieldEncoder Trait
`selium-guest::encoding` SHALL provide a `FieldEncoder` trait for encoding individual fields of a schema type into Flatbuffers builders.

```rust
pub trait FieldEncoder {
    type Output<'bldr>;
    fn encode_field<'bldr, A: flatbuffers::Allocator + 'bldr>(
        &self,
        builder: &mut FlatBufferBuilder<'bldr, A>,
    ) -> Self::Output<'bldr>;
}
```

#### Scenario: Nested schema type encodes as field
- **WHEN** a struct containing a nested `#[schema]` field is encoded
- **THEN** the nested field SHALL be written as a Flatbuffers `WIPOffset` via `FieldEncoder::encode_field`

### Requirement: SchemaDescriptor Type
`selium-guest::encoding` SHALL provide a `SchemaDescriptor` struct with two fields:
- `fqname: &'static str` — the fully-qualified Flatbuffers type name (e.g., `"selium.live_table.LiveTableMessage"`)
- `hash: [u8; 16]` — the first 16 bytes of the BLAKE3 hash of the `.fbs` file content

#### Scenario: Schema descriptor is const-constructible
- **WHEN** a `SchemaDescriptor` is defined as a `const` item
- **THEN** it SHALL be usable in const contexts for static schema registration

### Requirement: Flatbuffers Code Generation from .fbs Files
`selium-guest`'s `build.rs` SHALL compile `.fbs` schema files into Rust source files using `flatc-fork` and `flatbuffers-build`.

The build script SHALL:
- Emit `cargo:rerun-if-changed=schemas/` so schemas are tracked for rebuild
- Generate Rust bindings into `src/fbs/` organized by namespace (e.g., `src/fbs/selium/live_table/`)
- Auto-generate `src/fbs/mod.rs` to re-export all generated modules

#### Scenario: Schema change triggers rebuild
- **WHEN** a `.fbs` file in the `schemas/` directory is modified
- **THEN** cargo SHALL re-run the build script and regenerate the Flatbuffers bindings

#### Scenario: Generated bindings are importable
- **WHEN** a guest module imports `crate::fbs::selium::live_table::LiveTableMessage`
- **THEN** the import SHALL resolve to the generated Flatbuffers table type with `root_as_live_table_message()`, `LiveTableMessageArgs`, and `LiveTableMessageBuilder`

### Requirement: #[schema] Proc Macro for Structs
`selium-guest-macros` SHALL provide a `#[schema]` attribute macro that bridges a Rust struct to a Flatbuffers table. The macro SHALL accept three parameters:

| Parameter | Description |
|-----------|-------------|
| `path` | Path to the `.fbs` file relative to the crate root |
| `ty` | Fully-qualified Flatbuffers type name (e.g., `"selium.live_table.LiveTableMessage"`) |
| `binding` | Path to the generated Flatbuffers binding struct (e.g., `crate::fbs::selium::live_table::LiveTableMessage`) |

When applied to a struct with named fields, the macro SHALL generate:
1. A `const {Type}Schema: SchemaDescriptor` with the fqname and BLAKE3 hash
2. A `HasSchema` impl linking the type to its schema descriptor
3. A `FieldEncoder` impl for use as a nested field in parent tables
4. A `new()` constructor from all fields
5. A `write_flatbuffer()` method that serializes the struct into a Flatbuffers builder
6. A `from_flatbuffer()` method that deserializes from a Flatbuffers view
7. A `FlatMsg` impl providing `encode()` and `decode()`

#### Scenario: Struct with scalar fields
- **WHEN** a struct with `u32`, `u64`, `bool`, `String`, and `Vec<u8>` fields is annotated with `#[schema(path = "...", ty = "...", binding = "...")]`
- **THEN** `FlatMsg::encode` SHALL produce valid Flatbuffers bytes and `FlatMsg::decode` SHALL reconstruct the original value

#### Scenario: Struct with nested schema field
- **WHEN** a struct contains a field whose type also has `#[schema]` (implements `FieldEncoder`)
- **THEN** the outer struct's `write_flatbuffer` SHALL encode the nested field via `FieldEncoder::encode_field` and `FlatMsg::decode` SHALL reconstruct the nested value via `from_flatbuffer`

#### Scenario: Struct with optional fields
- **WHEN** a struct contains `Option<String>` or `Option<Vec<u8>>` fields
- **THEN** `write_flatbuffer` SHALL encode `None` as a missing Flatbuffers field and `Some(value)` as the encoded value

#### Scenario: Schema hash changes when .fbs file changes
- **WHEN** the `.fbs` file referenced by `path` is modified
- **THEN** the generated `SchemaDescriptor` SHALL contain a different hash value on the next compilation

### Requirement: #[schema] Proc Macro for Enums
`selium-guest-macros`'s `#[schema]` macro SHALL support Rust enums where:
- All variants are unit variants (mapped 1:1 to Flatbuffers enum variants)
- At most one variant is a tuple variant with a single field (acts as a fallback for unknown values)

The generated code SHALL include:
1. A `HasSchema` impl
2. A `FieldEncoder` impl (returning the Flatbuffers enum value directly)
3. `write_flatbuffer()` mapping Rust variants to Flatbuffers enum values
4. `from_flatbuffer()` mapping Flatbuffers enum values to Rust variants, with the fallback variant capturing unrecognized values

#### Scenario: Unit-only enum round-trip
- **WHEN** a `#[schema]`-annotated enum with only unit variants is encoded and decoded
- **THEN** all known variants SHALL round-trip correctly

#### Scenario: Fallback variant captures unknown values
- **WHEN** a Flatbuffers message contains an enum value not matching any known variant
- **THEN** `from_flatbuffer` SHALL map it to the fallback tuple variant (e.g., `Unknown(i8)`) instead of panicking

### Requirement: Primitives Implement FlatMsg
`selium-guest::encoding` SHALL provide `FlatMsg` and `HasSchema` implementations for common primitive types used as message fields: `u32`, `i32`, `u64`, `String`, `()`, and `Vec<u8>`.

#### Scenario: u32 round-trips
- **WHEN** `FlatMsg::encode(&42u32)` is decoded via `FlatMsg::decode`
- **THEN** the result SHALL be `Ok(42u32)`

#### Scenario: String round-trips
- **WHEN** `FlatMsg::encode(&"hello".to_string())` is decoded via `FlatMsg::decode`
- **THEN** the result SHALL be `Ok("hello".to_string())`

#### Scenario: Vec<u8> round-trips
- **WHEN** `FlatMsg::encode(&vec![1u8, 2, 3])` is decoded via `FlatMsg::decode`
- **THEN** the result SHALL be `Ok(vec![1u8, 2, 3])`

### Requirement: Schema Files
`selium-guest` SHALL include a `schemas/` directory containing `.fbs` schema files for shared message types. At minimum, `schemas/live_table.fbs` SHALL define the `LiveTableMessage` table with `mutation_id: ulong`, `key_bytes: [ubyte]`, `value_bytes: [ubyte]`, and `expected_version: ulong` fields.

#### Scenario: Schema file is present and valid
- **WHEN** `flatc-fork` compiles `schemas/live_table.fbs`
- **THEN** it SHALL produce valid Rust bindings without errors

### Requirement: encode_typed / decode_typed Use FlatMsg
`selium-guest::codec` SHALL provide `encode_typed<T: FlatMsg>` and `decode_typed<T: FlatMsg>` functions that compose frame-level encoding/decoding with `FlatMsg` serialization. These SHALL replace the existing rkyv-based implementations.

#### Scenario: Typed codec round-trip
- **WHEN** a `FlatMsg`-implementing type is encoded via `encode_typed` and decoded via `decode_typed`
- **THEN** the decoded value SHALL equal the original and the frame-level framing (length prefix) SHALL be valid
