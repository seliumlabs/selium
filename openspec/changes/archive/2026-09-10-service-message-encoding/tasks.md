# Tasks

Stages are ordered so the workspace compiles at every check-point. Each stage ends green (`cargo check --workspace`, `cargo test --workspace --all-targets`).

## 1. Crate relocation (no behaviour change)

- [x] 1.1 Rename package `selium-encoding` → `selium-service` (directory `crates/encoding` → `crates/service`); update the workspace Cargo.toml entry and the dependent Cargo.tomls (`shm`, `wire`, `runtime`, `guest`, `client`, `proto-http`, `proto-dns`, `control-plane`)
- [x] 1.2 Update `extern crate self as selium_encoding` → `selium_service` in the crate root
- [x] 1.3 Update the schema macro `encoding_path()` `CARGO_CRATE_NAME` branches: add `selium_service => crate`; change `selium_wire`, `selium_proto_http`, `selium_proto_dns` to resolve `selium_service`; drop or fix the stale `selium_guest => crate::encoding` branch
- [x] 1.4 Update `binding = "selium_encoding::…"` literals and the `../encoding/schemas/…` relative path in `crates/wire/src/tables.rs` (and any other schema-path literal) to `selium_service` / `../service/…`
- [x] 1.5 Move the service message types from `crates/abi/src/lib.rs` into `crates/service`: `InterfaceMetadata`, `ResourceTarget`, `DiscoveryRequest`, `DiscoveryResponse`, `Deployment`, `PipelineBinding`, `DesiredStateRecord`, `ControlRequest`, `ResolvedTarget`, `DelegationStatus`, `ControlResponse`, `SchedulerRequest`, `SchedulerResponse`. Keep their rkyv derives for this stage so existing rkyv call sites keep compiling
- [x] 1.6 Update `selium-guest` re-exports (`DiscoveryRequest` etc., `FlatMsg`/`HasSchema`/`FieldEncoder`, `codec`) from `selium_encoding` to `selium_service`, keeping the same pub names
- [x] 1.7 Fix every import site of the moved types across `runtime`, `guests/*`, `client`, `wire`, `shm` to use `selium_service`
- [x] 1.8 Update `crates/encoding` tests and `runtime/tests` that name `selium_encoding`/moved types; confirm `cargo test --workspace --all-targets` passes

## 2. Switch the codec (feed + durable log to FlatBuffers)

- [x] 2.1 Extend `crates/service/schemas/discovery.fbs` to represent `Register.owner` (optional `ulong`), `RevokeByOwner { process_id }`, and `SeedDomain { domain, tenant }` with new variant tags; regenerate bindings; update `DiscoveryRequestWire`/`From`/`try_from` to round-trip them (remove the Tier-1 collapse to `Resolve`)
- [x] 2.2 Extend `crates/service/schemas/control.fbs` with `DesiredStateRecord` (variant + `deployment` + `pipeline` + `workload_id`) and `PipelineBinding { name, from, to }`; add their wire types and strict decode
- [x] 2.3 Change the runtime discovery publisher (`DiscoveryPublisher`) to `Publisher<DiscoveryRequest, ShmTransport>` and `publish_discovery_event` to take a typed `DiscoveryRequest`; update producers in `runtime/src/{bootstrap,process,hostcall}.rs`
- [x] 2.4 Change the discovery guest feed subscriber to `Subscriber<DiscoveryRequest, ShmTransport>`; drop `decode_rkyv` from `feed_loop`
- [x] 2.5 Change the control plane's `record()`/`rebuild()` to encode and decode `DesiredStateRecord` via `FlatMsg`; drop rkyv from the durable log path
- [x] 2.6 Remove rkyv derives (`Archive`/`Serialize`/`Deserialize`) from the relocated service types now that no production path uses them; keep `#[rkyv]` only on the `selium-abi` hostcall/vocabulary types
- [x] 2.7 Update runtime tests (`discovery.rs`, `discovery_taxonomy.rs`, `guest_log_transport.rs`) and the `process.rs` feed-drain helper to decode typed `DiscoveryRequest` values instead of `decode_rkyv`
- [x] 2.8 Add round-trip tests for the new feed-only variants (`RevokeByOwner`, `SeedDomain`) and `DesiredStateRecord`/`PipelineBinding`

## 3. Schema macro: enum codec generation

- [x] 3.1 Add the data-enum path to `crates/guest/macros/src/schema.rs` (new `expand_data_enum` alongside the existing unit-enum path), reusing the struct path's field encode/decode emission over the table binding's `Args`/`view`
- [x] 3.2 Implement tag assignment by declaration order with optional `#[tag(n)]` override and duplicate/out-of-range compile errors
- [x] 3.3 Implement field mapping: default name equality, `#[field("…")]` rename, required-name on single unnamed-field variants, `#[schema(skip)]` (encode-omit, decode-default)
- [x] 3.4 Implement mixed unit + data variants, and strict decode (unknown tag and missing-required-field → `InvalidFlatbuffer`)
- [x] 3.5 Add ui-pass / unit tests for each attribute case and the strict-decode errors
- [x] 3.6 Convert `SchedulerRequest`/`SchedulerResponse` to `#[schema]` enums; delete their wire structs and hand conversions
- [x] 3.7 Convert `ControlRequest`/`ControlResponse` to `#[schema]` enums; delete `ControlRequestWire`/`ControlResponseWire` and `control_*_try_from_wire`
- [x] 3.8 Convert `DiscoveryRequest`/`DiscoveryResponse` to `#[schema]` enums; delete their wire structs and `discovery_*_try_from_wire`
- [x] 3.9 Confirm strict-decode behaviour is preserved by the generated code (existing unknown-tag/missing-target tests still pass)

## 4. Pipe control frames to FlatBuffers

- [x] 4.1 Add a `pipe-control` schema (`crates/service/schemas/pipe_control.fbs`) and move `PipeControl` from `crates/wire/src/control.rs` into `selium-service` as a `#[schema]` enum (uses the stage-3 macro)
- [x] 4.2 Re-export `PipeControl` and its termination codes from `selium-wire` so the `wire`/`client`/`bridge-channel` imports are unchanged externally
- [x] 4.3 Switch `crates/client` handshake (`write_handshake`/`await_acceptance`) and `guests/bridge-channel` (`bridge_pipe`, `terminate`, accepted replies) from `PipeControl::encode`/`decode` (rkyv) to `FlatMsg::encode`/`FlatMsg::decode`
- [x] 4.4 Update bridge-channel and client tests to construct/assert `PipeControl` via `FlatMsg`

## 5. Cleanup and verification

- [x] 5.1 Grep for and remove any remaining `decode_rkyv`/`encode_rkyv` uses outside the hostcall ABI path (`abi`, `guest/src/hostcall.rs`, `runtime/src/host_functions.rs`, and their tests)
- [x] 5.2 Run `cargo fmt --all`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace --all-targets`
- [x] 5.3 Build the WASM system guests (`cargo build --target wasm32-unknown-unknown -p selium-spine-demo -p selium-discovery`) and run the golden-path spine test (`cargo test -p selium-runtime --test spine -- --ignored`)
- [x] 5.4 Update `README.md` and any doc comments that still name `selium-encoding` or describe service types as part of `selium-abi`
