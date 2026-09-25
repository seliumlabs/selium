## ADDED Requirements

### Requirement: Host-Guest Boundary Only

`selium-abi` SHALL define only the host↔guest hostcall contract — the hostcall request, envelope, output, and completion types, and the resource descriptors — plus the shared capability and resource vocabulary those hostcalls carry (`Capability`, `CapabilityGrant`, `ResourceSelector`, `ResourceIdentity`, `LocalityScope`, `ScopeContext`, `ResourceClass`, `ResourceKind`). Service-level message types (discovery, control-plane, and scheduler messages, deployment and pipeline desired-state records, resource and interface metadata, and pipe-control frames) SHALL NOT be defined in `selium-abi`; they live in `selium-service`, which depends on `selium-abi` for the shared vocabulary.

#### Scenario: Service messages are not part of the ABI

- **WHEN** a consumer wants a service message such as `DiscoveryRequest` or `ControlResponse`
- **THEN** `selium-abi` SHALL NOT export it
- **AND** the consumer SHALL import it from `selium-service`

#### Scenario: Hostcall contract is unchanged

- **WHEN** a guest encodes `HostcallRequest` or `HostcallEnvelope`
- **THEN** it SHALL use the rkyv helpers in `selium-abi` exactly as before

## REMOVED Requirements

### Requirement: ResourceTarget Classification and Labels (REMOVED)

**Reason**: `ResourceTarget` is a service message, not a hostcall type. It moves to `selium-service` with FlatBuffers encoding; classification and labels round-tripping is re-specified there under "Single Canonical Type Per Message" and "Strict FlatBuffers Service Decode".

### Requirement: Discovery Enumeration Query Variants (REMOVED)

**Reason**: `DiscoveryRequest`/`DiscoveryResponse` are service messages. Their prefix, label-query, and multi-target variants are re-specified under `selium-service` with FlatBuffers encoding, not rkyv.

### Requirement: Strict FlatBuffers Wire Decode (REMOVED)

**Reason**: This concern belongs to the FlatBuffers codec, which lives in `selium-service`. Strict decode behaviour is re-specified in `selium-service` under "Strict FlatBuffers Service Decode".
