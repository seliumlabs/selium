## ADDED Requirements

### Requirement: Schema Proc Macro
`selium-guest-macros` SHALL provide a `#[schema]` attribute macro that bridges idiomatic Rust structs and enums to Flatbuffers bindings generated from `.fbs` schema files.

The macro SHALL accept three required parameters:
- `path`: A string literal or expression resolving to the path of the `.fbs` file relative to `CARGO_MANIFEST_DIR`
- `ty`: A string literal specifying the fully-qualified Flatbuffers type name (e.g., `"selium.logging.LogRecord"`)
- `binding`: A path to the generated Flatbuffers binding struct (e.g., `crate::fbs::selium::logging::LogRecord`)

At compile time, the macro SHALL:
1. Read the `.fbs` file content at the given `path`
2. Compute a BLAKE3 hash of the file content and extract the first 16 bytes
3. Generate a `const {Type}Schema: SchemaDescriptor` with the fqname and hash
4. Generate a `HasSchema` impl for the annotated type
5. Generate `write_flatbuffer()` and `from_flatbuffer()` methods
6. Generate a `FlatMsg` impl providing `encode()` and `decode()`
7. Generate a `FieldEncoder` impl for use as a nested field in parent Flatbuffers tables

#### Scenario: Struct with named fields
- **WHEN** a struct with named fields (scalars, `String`, `Vec<u8>`, `Option<T>`, nested `#[schema]` types) is annotated with `#[schema(...)]`
- **THEN** the generated `FlatMsg::encode` SHALL produce valid Flatbuffers bytes and `FlatMsg::decode` SHALL reconstruct the original struct value

#### Scenario: Enum with unit variants
- **WHEN** an enum with only unit variants is annotated with `#[schema(...)]` where the corresponding Flatbuffers enum has matching variants
- **THEN** the generated `write_flatbuffer` SHALL map each Rust variant to the corresponding Flatbuffers enum variant, and `from_flatbuffer` SHALL perform the reverse mapping

#### Scenario: Enum with fallback variant
- **WHEN** an enum has unit variants plus one tuple variant (e.g., `Unknown(i8)`) and is annotated with `#[schema(...)]`
- **THEN** `from_flatbuffer` SHALL map unrecognized Flatbuffers enum values to the fallback variant, preserving the raw discriminant

#### Scenario: Schema hash reflects .fbs content
- **WHEN** the `.fbs` file referenced by `path` is modified between compilations
- **THEN** the generated `SchemaDescriptor::hash` SHALL differ, enabling compile-time detection of schema changes

#### Scenario: Macro rejects unsupported types
- **WHEN** `#[schema]` is applied to a struct with unnamed fields (tuple struct) or an enum with multiple tuple variants
- **THEN** the macro SHALL produce a compile error with a descriptive message

### Requirement: Schema Macro supports concat! and env! in path
The `#[schema]` macro's `path` parameter SHALL support `concat!` and `env!` macro expressions in addition to plain string literals, enabling path construction from environment variables or compile-time constants.

#### Scenario: Path uses concat!
- **WHEN** `path = concat!(env!("CARGO_MANIFEST_DIR"), "/schemas/logging.fbs")` is used
- **THEN** the macro SHALL resolve the full path and read the schema file correctly

### Requirement: Flatbuffers and blake3 Dependencies
`selium-guest-macros` SHALL depend on `blake3` for computing schema content hashes at compile time. The `flatbuffers` crate is a transitive dependency via `selium-guest` — the macro generates code that references `flatbuffers` types but does not depend on `flatbuffers` directly.

#### Scenario: Macro compiles with blake3
- **WHEN** `selium-guest-macros` is compiled
- **THEN** it SHALL successfully compute BLAKE3 hashes of `.fbs` files referenced in `#[schema]` attributes
