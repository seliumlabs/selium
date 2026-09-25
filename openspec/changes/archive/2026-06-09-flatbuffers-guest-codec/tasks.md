## 1. Workspace Dependencies

- [x] 1.1 Add `flatbuffers = "25.12"` to workspace `Cargo.toml` dependencies
- [x] 1.2 Add `flatbuffers-build = "0.2"` to workspace `Cargo.toml` dependencies
- [x] 1.3 Add `flatc-fork = "0.6.0"` to workspace `Cargo.toml` dependencies
- [x] 1.4 Add `blake3 = "1.8"` to workspace `Cargo.toml` dependencies
- [x] 1.5 Add `flatbuffers` to `selium-guest/Cargo.toml` dependencies
- [x] 1.6 Add `flatbuffers` to `selium-guest/Cargo.toml` dev-dependencies (for tests)
- [x] 1.7 Add `flatbuffers-build` and `flatc-fork` to `selium-guest/Cargo.toml` build-dependencies
- [x] 1.8 Add `blake3` to `selium-guest-macros/Cargo.toml` dependencies

## 2. Schema Files and Build Script

- [x] 2.1 Create `crates/core/guest/schemas/` directory
- [x] 2.2 Create `schemas/live_table.fbs` with `LiveTableMessage` table (mutation_id, key_bytes, value_bytes, expected_version)
- [x] 2.3 Create `crates/core/guest/build.rs` that compiles `.fbs` files via `flatc-fork` + `flatbuffers-build` into `src/fbs/`
- [x] 2.4 Implement `rewrite_module_root` in build.rs to auto-generate `src/fbs/mod.rs`
- [x] 2.5 Add `cargo:rerun-if-changed=schemas/` to build.rs
- [x] 2.6 Verify generated bindings compile and are importable as `crate::fbs::selium::live_table::LiveTableMessage`

## 3. Encoding Module (Traits and Primitives)

- [x] 3.1 Create `crates/core/guest/src/encoding.rs` module
- [x] 3.2 Define `SchemaDescriptor` struct with `fqname: &'static str` and `hash: [u8; 16]`
- [x] 3.3 Define `HasSchema` trait with `const SCHEMA: SchemaDescriptor`
- [x] 3.4 Define `FlatMsg` trait with `encode(&Self) -> Vec<u8>` and `decode(&[u8]) -> Result<Self, InvalidFlatbuffer>`
- [x] 3.5 Define `FieldEncoder` trait with `Output<'bldr>` GAT and `encode_field` method
- [x] 3.6 Implement `FlatMsg` and `HasSchema` for `()`
- [x] 3.7 Implement `FlatMsg` and `HasSchema` for `u32`
- [x] 3.8 Implement `FlatMsg` and `HasSchema` for `i32`
- [x] 3.9 Implement `FlatMsg` and `HasSchema` for `u64`
- [x] 3.10 Implement `FlatMsg` and `HasSchema` for `String`
- [x] 3.11 Implement `FlatMsg` and `HasSchema` for `Vec<u8>`
- [x] 3.12 Add `pub mod encoding` to `crates/core/guest/src/lib.rs`
- [x] 3.13 Re-export `SchemaDescriptor`, `HasSchema`, `FlatMsg`, `FieldEncoder` from `selium-guest` crate root

## 4. #[schema] Proc Macro

- [x] 4.1 Add `blake3` to `selium-guest-macros/Cargo.toml` dependencies
- [x] 4.2 Create `crates/core/guest/macros/src/schema.rs` with the `expand` entry point
- [x] 4.3 Implement attribute parsing (`path`, `ty`, `binding` parameters) with `concat!` and `env!` support
- [x] 4.4 Implement `.fbs` file reading and BLAKE3 hash computation
- [x] 4.5 Implement `expand_struct` — generates `SchemaDescriptor` const, `HasSchema` impl, `FieldEncoder` impl, `new()`, `write_flatbuffer()`, `from_flatbuffer()`, `FlatMsg` impl
- [x] 4.6 Implement `encode_field` helper for scalars, `String`, `Vec<u8>`, `Vec<String>`, `Vec<scalar>`, `Vec<schema>`, `Option<T>`, nested schema types
- [x] 4.7 Implement `decode_field` helper (inverse of encode)
- [x] 4.8 Implement `expand_enum` — unit variants + optional single-tuple fallback variant
- [x] 4.9 Register the `#[schema]` attribute macro in `crates/core/guest/macros/src/lib.rs`
- [x] 4.10 Re-export the `schema` macro from `selium-guest` (via `pub use selium_guest_macros::schema`)

## 5. Codec Layer Migration

- [x] 5.1 Rewrite `codec.rs`: replace `encode_typed<T: RkyvEncode>` with `encode_typed<T: FlatMsg>` using `frame_bytes(&FlatMsg::encode(value))`
- [x] 5.2 Rewrite `codec.rs`: replace `decode_typed<T: Archive + Deserialize + CheckBytes>` with `decode_typed<T: FlatMsg>` using `FlatMsg::decode(deframe_bytes(bytes)?)`
- [x] 5.3 Update `codec.rs` tests to use `#[schema]`-annotated test struct instead of rkyv derives
- [x] 5.4 Remove `rkyv` imports from `codec.rs`

## 6. Pub/Sub Migration

- [x] 6.1 Change `Publisher<T>` trait bounds: `T: RkyvEncode` → `T: FlatMsg`
- [x] 6.2 Update `Publisher::publish`: use `FlatMsg::encode` instead of `encode_rkyv`
- [x] 6.3 Update `Sink<T> impl for Publisher<T>`: use `FlatMsg::encode` instead of `encode_rkyv`
- [x] 6.4 Change `Subscriber<T>` trait bounds: remove rkyv bounds, add `T: FlatMsg`
- [x] 6.5 Update `Subscriber::read_with_writer_id`: use `FlatMsg::decode` instead of `decode_rkyv`
- [x] 6.6 Update `Stream impl for Subscriber<T>`: use `FlatMsg::decode` instead of `decode_rkyv`
- [x] 6.7 Update pub/sub tests to use `#[schema]`-annotated test types instead of rkyv derives
- [x] 6.8 Remove rkyv imports from `io/pubsub.rs`

## 7. RPC Migration

- [x] 7.1 Change `RpcClient<Req, Rep>` bounds: `Req: FlatMsg`, `Rep: FlatMsg`
- [x] 7.2 Update `RpcClient::request`: use `FlatMsg::encode` for request, `FlatMsg::decode` for reply
- [x] 7.3 Change `RpcConnection<Req, Rep>` bounds: `Req: FlatMsg`, `Rep: FlatMsg`
- [x] 7.4 Update `RpcRequest::payload` and `into_payload`: use `FlatMsg::decode`
- [x] 7.5 Update `RpcRequest::reply`: use `FlatMsg::encode`
- [x] 7.6 Update `RpcAccept<Req, Rep>` bounds: `Req: FlatMsg`, `Rep: FlatMsg`
- [x] 7.7 Update `try_read_reply`: use `FlatMsg::decode` instead of `decode_rkyv`
- [x] 7.8 Update `create_test_pair` bounds: remove rkyv bounds, add `FlatMsg`
- [x] 7.9 Update RPC tests to use `#[schema]`-annotated test types instead of rkyv derives
- [x] 7.10 Remove rkyv imports from `io/rpc.rs`

## 8. Live Tables Migration

- [x] 8.1 Create `LiveTableMessageWire` struct with `#[schema(path = "schemas/live_table.fbs", ty = "selium.live_table.LiveTableMessage", binding = "crate::fbs::selium::live_table::LiveTableMessage")]` and fields: `mutation_id: u64`, `key_bytes: Vec<u8>`, `value_bytes: Vec<u8>`, `expected_version: u64`
- [x] 8.2 Define `LiveTableMessage<K, V>` with `mutation_id`, `key: K`, `value: Option<V>`, `expected_version: Option<u64>`
- [x] 8.3 Implement `FlatMsg for LiveTableMessage<K, V> where K: FlatMsg, V: FlatMsg` — encodes K/V to bytes, wraps in wire type, decodes in reverse
- [x] 8.4 Implement `HasSchema for LiveTableMessage<K, V>` delegating to `LiveTableMessageWireSchema`
- [x] 8.5 Change `LiveTable<K, V>` bounds: `K: FlatMsg + Clone + Eq + Hash`, `V: FlatMsg + Clone`
- [x] 8.6 Update `LiveTable::set`: encode K and V via `FlatMsg`, wrap in `LiveTableMessage`, publish
- [x] 8.7 Update `LiveTable::sync` and `sync_until_own_mutation`: decode `LiveTableMessage` via `FlatMsg`, then decode K and V from bytes
- [x] 8.8 Update `LiveTable::compare_and_set` and `delete` to use new encoding
- [x] 8.9 Update `apply_message_to` to receive decoded `LiveTableMessage<K, V>` (already decoded)
- [x] 8.10 Update table tests — use `FlatMsg`-implementing test types for K and V
- [x] 8.11 Remove rkyv derives from `LiveTableMessage` and rkyv imports from `io/tables.rs`

## 9. Guest Crate Migration

- [x] 9.1 Update `crates/guests/cluster` message types: replace rkyv derives with `#[schema]` + Flatbuffers schemas (or inline `FlatMsg` impls if simple enough)
- [x] 9.2 Update `crates/guests/discovery` message types similarly
- [x] 9.3 Update `crates/guests/external-api` message types similarly
- [x] 9.4 Update `crates/guests/scheduler` message types similarly
- [x] 9.5 Update `crates/guests/supervisor` message types similarly
- [x] 9.6 Verify all guest crates compile with new codec

## 10. Cleanup and Verification

- [x] 10.1 Remove `rkyv` from `selium-guest/Cargo.toml` direct dependencies (it remains transitively via `selium-abi`)
- [x] 10.2 Run `cargo test -p selium-guest` — all tests pass
- [x] 10.3 Run `cargo test -p selium-guest-macros` — all tests pass
- [x] 10.4 Run `cargo test --workspace` — full workspace builds and tests pass
- [x] 10.5 Verify `rkyv` is still used in `hostcall.rs` and `selium-abi` (no regression)
- [x] 10.6 Run `cargo clippy --workspace` — no new warnings
