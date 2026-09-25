## Why

The guest-side I/O codec currently uses `rkyv` for all serialization (pub/sub, RPC, live tables). While `rkyv` works well for the internal ABI boundary between guest and host, it is a Rust-only serialization framework with an implicit schema defined entirely by Rust type definitions. This makes it impossible to share message schemas with non-Rust consumers, precludes schema versioning, and prevents cross-language interop — a core requirement for a platform where guests may be written in multiple languages and where message schemas need to evolve independently of the codec implementation.

## What Changes

- **BREAKING**: Replace `rkyv`-based serialization in the guest I/O codec layer (`encode_typed`/`decode_typed`, `Publisher<T>`, `Subscriber<T>`, `RpcClient<Req,Rep>`, `RpcConnection<Req,Rep>`, `LiveTable<K,V>`) with Flatbuffers-based schema-driven serialization
- Add `.fbs` schema files as canonical, versionable, cross-language message definitions
- Add a `#[schema]` proc macro that bridges idiomatic Rust types to generated Flatbuffers bindings
- Add `FlatMsg`, `HasSchema`, and `FieldEncoder` traits providing the encode/decode contract
- Add `SchemaDescriptor` (fully-qualified name + 16-byte BLAKE3 content hash) for content-addressable schema identification
- Keep `rkyv` for the hostcall/ABI boundary (`hostcall.rs`, `selium-abi` types, runtime host-side decoding) — the internal guest↔host transport is unchanged
- The framing layer (`FrameHeader`, `FramedRead`, `FramedWrite`, ring buffer I/O) is unchanged — it operates on raw bytes and is codec-agnostic

## Capabilities

### New Capabilities
- `flatbuffers-codec`: Schema-driven Flatbuffers codec infrastructure including `FlatMsg`/`HasSchema`/`FieldEncoder` traits, `SchemaDescriptor` type, `#[schema]` proc macro, `.fbs` schema files, and `build.rs` Flatbuffers code generation

### Modified Capabilities
- `selium-guest`: `Publisher<T>`, `Subscriber<T>`, `RpcClient<Req,Rep>`, `RpcConnection<Req,Rep>`, and `LiveTable<K,V>` trait bounds switch from `rkyv` (`Archive`/`Serialize`/`Deserialize`/`CheckBytes`) to `FlatMsg`; `encode_typed`/`decode_typed` use `FlatMsg` instead of rkyv
- `selium-rpc`: Typed RPC client and connection requirements updated to reference Flatbuffers encoding instead of rkyv encoding
- `selium-guest-macros`: New `#[schema]` proc macro attribute for bridging Rust structs/enums to Flatbuffers bindings with `HasSchema` and `FlatMsg` impl generation

## Impact

- **Affected crates**: `selium-guest`, `selium-guest-macros`, all five `crates/guests/*` crates (cluster, discovery, external-api, scheduler, supervisor)
- **New workspace dependencies**: `flatbuffers = "25.12"`, `flatbuffers-build`, `flatc-fork`, `blake3`
- **Removed dependency**: `rkyv` from `selium-guest` (it remains transitively via `selium-abi` for hostcalls)
- **Schema files**: New `schemas/` directory in `selium-guest` with `.fbs` files for shared message types
- **User-facing break**: Guest code that currently uses `#[derive(Archive, Serialize, Deserialize)]` for message types must switch to `#[derive(Clone, Debug, PartialEq)]` + `#[schema(path = "...", ty = "...", binding = "...")]` with corresponding `.fbs` schema definitions
