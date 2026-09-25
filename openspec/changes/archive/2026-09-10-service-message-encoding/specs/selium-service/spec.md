## Purpose

`selium-service` is the single authority for fabric **service** message types and their FlatBuffers encoding. It replaces the `selium-encoding` crate name and absorbs the service-level message types previously defined in `selium-abi`. `selium-abi` remains the sole host↔guest (hostcall) contract.

## ADDED Requirements

### Requirement: Service Message Crate

The workspace SHALL provide a `selium-service` crate owning the canonical Rust types for fabric service messages — the discovery, control-plane, and scheduler request/response messages, deployment and pipeline desired-state records, resource and interface metadata, and per-stream pipe-control frames — together with their `.fbs` schemas, generated FlatBuffers bindings, and `FlatMsg`/`HasSchema` implementations. The crate SHALL depend on `selium-abi` for the shared capability/resource vocabulary (`Capability`, `ResourceClass`, `ResourceIdentity`, and so on) and SHALL NOT be named `selium-encoding`.

#### Scenario: Crate renamed and authoritative

- **WHEN** a consumer needs a service message type such as `ControlResponse`
- **THEN** it SHALL import it from `selium-service`
- **AND** `selium-abi` SHALL NOT export service message types

### Requirement: Single Canonical Type Per Message

Each service message SHALL have exactly one hand-maintained canonical Rust type. Record-style messages (`ResourceTarget`, `InterfaceMetadata`, `Deployment`, `ResolvedTarget`, and their label/domain-entry companions) SHALL be `#[schema]` structs carrying their own `FlatMsg` implementation, with no duplicate rkyv-derived type in `selium-abi` and no hand-written wire-mirror struct.

#### Scenario: Record message has one type

- **WHEN** `Deployment` is defined in `selium-service`
- **THEN** it SHALL be a single `#[schema]` struct whose encode and decode use its generated `FlatMsg`
- **AND** no other `Deployment` type SHALL exist in the workspace

### Requirement: Data-Carrying Enums Encode via Generated Codec

For a data-carrying enum annotated with `#[schema(...)]` whose binding is a FlatBuffers table (flattened fields with a `variant: ubyte` discriminator), the enum SHALL be the canonical type and SHALL derive its `FlatMsg` and `HasSchema` implementations from the schema macro. No companion wire struct and no hand-written `From`/`into-wire` conversion SHALL be required.

#### Scenario: Control response encodes and decodes

- **WHEN** `ControlResponse::Accepted { workload_id, replicas, module, delegated }` is encoded via `FlatMsg::encode` and decoded via `FlatMsg::decode`
- **THEN** the decoded value SHALL equal the original without any intermediate wire struct

### Requirement: Desired-State Records Are FlatBuffers-Encodable

`Deployment`, `PipelineBinding`, and `DesiredStateRecord` SHALL be FlatBuffers-encodable via `FlatMsg` so the control plane can persist desired state to its durable log and replay it without using the `selium-abi` rkyv codec.

#### Scenario: Desired-state record round-trips

- **WHEN** a `DesiredStateRecord::Deployment(..)` value is encoded and decoded through `FlatMsg`
- **THEN** the decoded record SHALL equal the original
- **AND** no rkyv serialisation SHALL be involved

### Requirement: Pipe-Control Frames Are Service Messages

The per-stream pipe-control messages (`PipeControl::Handshake { uri }`, `Accepted`, and `Terminate { code }`) SHALL be defined in `selium-service` and SHALL encode as FlatBuffers on the bridge stream handshake, replacing the rkyv encoding.

#### Scenario: Handshake is FlatBuffers

- **WHEN** an external client opens a bridge stream and writes the pipe-control handshake
- **THEN** the handshake frame SHALL be a FlatBuffers-encoded `PipeControl::Handshake` payload
- **AND** the bridge channel SHALL decode it with `FlatMsg`, not `decode_rkyv`

### Requirement: Strict FlatBuffers Service Decode

The FlatBuffers codec for service messages SHALL decode strictly: an unknown resource-class segment, an unknown request/response variant tag, a `Register` without a target, a `Found` without a target, or a variant whose required field is absent SHALL fail the decode with an error rather than silently coercing to a default. Both in-fabric endpoints emit the closed vocabularies, so strictness never rejects legitimate traffic.

#### Scenario: Unknown variant tag fails decode

- **WHEN** a wire service message carries a variant tag outside the known set
- **THEN** decoding SHALL return an error

#### Scenario: Missing required nested field fails decode

- **WHEN** a `Found` response carries no target, or a `Register` request carries no target, or an `Accepted` response carries no deployment
- **THEN** decoding SHALL return an error rather than fabricate a default
