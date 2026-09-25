## ADDED Requirements

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
