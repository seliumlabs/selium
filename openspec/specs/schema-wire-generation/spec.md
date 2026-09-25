## Purpose

Generate wire types and FlatMsg bridge implementations automatically for generic domain types annotated with `#[schema(wire = WireTypeName)]`.

## Requirements

### Requirement: Wire type generation from generic domain type

When a struct with generic type parameters is annotated with `#[schema(wire = WireTypeName, ...)]`, the macro SHALL generate a wire struct named `WireTypeName` with all generic fields replaced by Flatbuffers-compatible types, and SHALL generate a `FlatMsg` impl on the domain type that bridges through the wire type.

#### Scenario: Generic field becomes bytes in wire type

- **WHEN** a struct `struct Msg<K> { pub field: K }` is annotated with `#[schema(wire = MsgWire, ...)]` and `K` is a generic type parameter
- **THEN** the generated wire struct SHALL have `pub field_bytes: Vec<u8>` in place of `pub field: K`

#### Scenario: Optional generic field becomes bytes in wire type

- **WHEN** a struct has `pub value: Option<V>` where `V` is a generic type parameter
- **THEN** the generated wire struct SHALL have `pub value_bytes: Vec<u8>`
- **AND** the bridge encode impl SHALL encode `Some(v)` as `FlatMsg::encode(&v)` and `None` as an empty `Vec`
- **AND** the bridge decode impl SHALL decode an empty `Vec` as `None` and non-empty as `Some(FlatMsg::decode(...)?)`

#### Scenario: Optional scalar field uses sentinel in wire type

- **WHEN** a struct has `pub version: Option<u64>` where `u64` is NOT a generic type parameter
- **THEN** the generated wire struct SHALL have `pub version: u64`
- **AND** the bridge encode impl SHALL use `value.unwrap_or(0)` for the sentinel
- **AND** the bridge decode impl SHALL convert `0` to `None` and non-zero to `Some(value)`

#### Scenario: Concrete field passes through unchanged

- **WHEN** a struct has `pub id: u64` where `u64` is NOT a generic type parameter
- **THEN** the generated wire struct SHALL have `pub id: u64` (same name, same type)

#### Scenario: String field passes through unchanged

- **WHEN** a struct has `pub name: String`
- **THEN** the generated wire struct SHALL have `pub name: String` (same name, same type)

### Requirement: FlatMsg bridge impl generation

The macro SHALL generate `impl<G: FlatMsg> FlatMsg for DomainType<G>` (one `FlatMsg` bound per generic parameter) that encodes the domain type by converting each field to its wire representation, constructing the wire struct, and delegating to the wire type's `FlatMsg::encode`, and decodes by delegating to the wire type's `FlatMsg::decode` then converting wire fields back to domain fields.

#### Scenario: Encode round-trips through generated bridge

- **WHEN** `LiveTableMessage<String, u64>` is encoded via the generated `FlatMsg::encode`
- **AND** then decoded via the generated `FlatMsg::decode`
- **THEN** the result SHALL equal the original value

#### Scenario: HasSchema delegates to wire type schema

- **WHEN** a domain type uses `#[schema(wire = FooWire, ...)]`
- **THEN** the generated `HasSchema` impl for the domain type SHALL reference `FooWireSchema` (the schema constant generated for the wire type)

### Requirement: Error on generic struct without wire parameter

If a struct has generic type parameters but the `#[schema]` annotation does NOT include `wire = ...`, the macro SHALL emit a compile error instructing the user to add the `wire` parameter.

#### Scenario: Generic struct without wire fails with clear error

- **WHEN** `struct Msg<K> { pub field: K }` is annotated with `#[schema(path = "...", ty = "...", binding = "...")]` without `wire = ...`
- **THEN** the macro SHALL produce a compile error
- **AND** the error message SHALL mention the `wire` parameter

### Requirement: Non-generic struct without wire works unchanged

When `#[schema]` is used on a struct WITHOUT generic type parameters and WITHOUT the `wire` parameter, the macro SHALL behave exactly as before (generating FlatMsg, HasSchema, FieldEncoder, new, write_flatbuffer, from_flatbuffer on the annotated type itself).

#### Scenario: Existing wire type annotation still compiles

- **WHEN** `InterfaceMetadataWire` is annotated with `#[schema(...)]` without `wire`
- **THEN** the macro SHALL generate the same code as before this change
- **AND** all existing tests SHALL pass unchanged

### Requirement: Data-carrying enum FlatMsg generation

When an enum with data-carrying variants is annotated with `#[schema(path = "...", ty = "...", binding = "…table…")]` where the binding is a FlatBuffers table with a `variant: ubyte` discriminator and flattened fields, the macro SHALL generate `FlatMsg` and `HasSchema` implementations directly on the enum. Encode SHALL build the table with the variant tag and only that variant's fields; decode SHALL read the tag and reconstruct the variant from the table's fields. The macro SHALL NOT require a companion wire struct or hand-written conversions, and SHALL NOT change the existing behaviour for unit-only enums bound to FlatBuffers *enum* types.

#### Scenario: Enum encodes as flattened table

- **WHEN** `ControlRequest::Resolve { uri }` is encoded
- **THEN** the macro-generated `FlatMsg::encode` SHALL write a table with `variant = 3` and the `uri` field, leaving the other fields absent

#### Scenario: Enum decodes from flattened table

- **WHEN** a table with `variant = 3` and a populated `uri` is decoded
- **THEN** the macro-generated `FlatMsg::decode` SHALL produce `ControlRequest::Resolve { uri }`

#### Scenario: Unit-only enum behaviour unchanged

- **WHEN** a unit-variant enum is annotated with `#[schema(...)]` as before
- **THEN** the macro SHALL generate the existing enum-binding codec, unchanged

### Requirement: Enum variant tag assignment

The macro SHALL assign variant tags in declaration order starting at zero. A `#[tag(n)]` attribute on a variant SHALL override the assigned tag; duplicate tag values or a tag outside the `u8` range SHALL be a compile error.

#### Scenario: Tags follow declaration order

- **WHEN** an enum `X { A, B, C }` is annotated with `#[schema(...)]`
- **THEN** `A` SHALL encode as tag `0`, `B` as `1`, and `C` as `2`

#### Scenario: Duplicate tag is a compile error

- **WHEN** two variants declare the same `#[tag(n)]` value
- **THEN** the macro SHALL emit a compile error

### Requirement: Enum field mapping attributes

A variant's field SHALL map to the table field with the same name by default. A `#[field("wire_name")]` attribute on a field SHALL map it to a differently-named table field. A single unnamed-field variant SHALL require `#[field("wire_name")]` to name its table field. A field annotated `#[schema(skip)]` SHALL be omitted on encode and defaulted (`Default`) on decode.

#### Scenario: Field rename maps to table field

- **WHEN** an enum field is annotated `#[field("bytes")]`
- **THEN** encode SHALL write that field to the table's `bytes` field and decode SHALL read it back from `bytes`

#### Scenario: Skipped field is omitted then defaulted

- **WHEN** a field is annotated `#[schema(skip)]`
- **THEN** encode SHALL leave its table field absent
- **AND** decode SHALL reconstruct it as the type's default (for example `Option` fields as `None`)

### Requirement: Enum strict decode errors

The macro-generated decode SHALL fail with an `InvalidFlatbuffer` when the `variant` tag is unknown, or when a variant's required (non-`Option`, non-skipped) field is absent from the table.

#### Scenario: Unknown tag rejected

- **WHEN** a decoded table carries a `variant` value outside the enum's tag range
- **THEN** decode SHALL return an `InvalidFlatbuffer` error

#### Scenario: Missing required field rejected

- **WHEN** the tag selects a variant whose required field the table does not carry
- **THEN** decode SHALL return a missing-required `InvalidFlatbuffer` error rather than a default

### Requirement: Mixed unit and data variants

An enum annotated with `#[schema(...)]` SHALL be allowed to mix unit variants with data-carrying variants against the same table binding. Unit variants SHALL encode as the tag alone with no fields, and SHALL decode from the tag alone.

#### Scenario: Unit variant round-trips alongside data variants

- **WHEN** `DiscoveryResponse::NotFound` (unit) and `DiscoveryResponse::Found(target)` (data) are both encoded and decoded
- **THEN** each SHALL round-trip to the same value
