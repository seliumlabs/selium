## ADDED Requirements

### Requirement: Wire parameter on schema macro

The `#[schema]` macro SHALL accept an optional `wire = WireTypeName` parameter. When present, the macro SHALL generate a wire struct and bridge `FlatMsg` impl instead of generating `FlatMsg`/`FieldEncoder`/`write_flatbuffer`/`from_flatbuffer` directly on the annotated type.

#### Scenario: Wire parameter triggers wire type generation

- **WHEN** a struct with generic parameters is annotated with `#[schema(path = "...", ty = "...", binding = "...", wire = MyWire)]`
- **THEN** the macro SHALL generate a struct named `MyWire` with the `#[schema]` annotation applied
- **AND** the macro SHALL generate a `FlatMsg` impl on the original struct that bridges through `MyWire`

#### Scenario: Wire parameter is optional

- **WHEN** a struct without generic parameters is annotated with `#[schema(path = "...", ty = "...", binding = "...")]` (no `wire`)
- **THEN** the macro SHALL behave exactly as before (direct `FlatMsg`/`FieldEncoder` generation on the annotated type)

### Requirement: Generic parameter auto-detection for byte fields

The macro SHALL automatically detect which struct fields reference generic type parameters. Fields whose type references a generic parameter SHALL be mapped to `Vec<u8>` in the generated wire struct with the `_bytes` suffix appended to the field name.

#### Scenario: Direct generic field detection

- **WHEN** `struct Msg<K> { pub key: K }` is processed
- **THEN** the macro SHALL identify `key` as a byte-serialized field because `K` is a generic parameter
- **AND** the generated wire field SHALL be `pub key_bytes: Vec<u8>`

#### Scenario: Option-wrapped generic field detection

- **WHEN** `struct Msg<V> { pub value: Option<V> }` is processed
- **THEN** the macro SHALL identify `value` as a byte-serialized field because `V` is a generic parameter inside `Option`
- **AND** the generated wire field SHALL be `pub value_bytes: Vec<u8>`

#### Scenario: Concrete type field is not detected as generic

- **WHEN** `struct Msg<K> { pub id: u64 }` is processed and `u64` is not a generic parameter
- **THEN** the macro SHALL NOT treat `id` as byte-serialized
- **AND** the generated wire field SHALL be `pub id: u64` (pass-through)

### Requirement: Bridge encode impl for generic types

The macro SHALL generate `FlatMsg::encode` on the domain type that: for each byte-serialized field, calls `FlatMsg::encode` on the field value to produce `Vec<u8>`; for each `Option<generic>` field, encodes `Some` values to bytes and `None` to empty `Vec`; for each `Option<scalar>` field, converts `None` to the zero sentinel; constructs the wire struct with all converted fields; and delegates to `FlatMsg::encode(&wire)`.

#### Scenario: Encode serializes generics to bytes

- **WHEN** `encode` is called on `Msg { key: "hello".to_string() }` where `key: K` and `K = String`
- **THEN** the generated code SHALL call `FlatMsg::encode(&value.key)` to produce the bytes for `key_bytes`

#### Scenario: Encode handles None option generics

- **WHEN** `encode` is called with `value: None` where `value: Option<V>`
- **THEN** the generated code SHALL set `value_bytes` to an empty `Vec`

#### Scenario: Encode handles None option scalars

- **WHEN** `encode` is called with `expected_version: None` where `expected_version: Option<u64>`
- **THEN** the generated code SHALL set `expected_version` to `0` in the wire struct

### Requirement: Bridge decode impl for generic types

The macro SHALL generate `FlatMsg::decode` on the domain type that: decodes the wire struct via `FlatMsg::decode(bytes)`; for each byte-serialized field, calls `FlatMsg::decode(&wire.field_bytes)` to reconstruct the typed value; for each `Option<generic>` field, returns `None` if bytes are empty and `Some(FlatMsg::decode(...))` otherwise; for each `Option<scalar>` field, converts the zero sentinel to `None`.

#### Scenario: Decode reconstructs generics from bytes

- **WHEN** `decode` is called with bytes containing an encoded `Msg<String>`
- **THEN** the generated code SHALL decode the wire struct, then decode `key_bytes` via `FlatMsg::decode::<String>(&wire.key_bytes)`

#### Scenario: Decode handles empty bytes as None

- **WHEN** `decode` encounters a wire struct with empty `value_bytes`
- **THEN** the generated code SHALL produce `value: None` in the domain struct

#### Scenario: Decode handles zero sentinel as None

- **WHEN** `decode` encounters a wire struct with `expected_version: 0`
- **THEN** the generated code SHALL produce `expected_version: None` in the domain struct
