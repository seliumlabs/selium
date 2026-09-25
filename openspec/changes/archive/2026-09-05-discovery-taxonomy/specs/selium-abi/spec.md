## ADDED Requirements

### Requirement: ResourceTarget Classification and Labels
`ResourceTarget` SHALL carry a resource class drawn from the closed `ResourceClass` enum, plus zero or more key/value label pairs. The class SHALL identify the typed segment of the target's URI (`proc`, `region`, `queue`, and so on), and SHALL round-trip through the rkyv codec unchanged.

#### Scenario: Target carries class and labels
- **WHEN** a `ResourceTarget` with class `Process` and labels `[("app","web")]` is encoded and decoded
- **THEN** the decoded target's class and labels SHALL equal the originals

### Requirement: Discovery Enumeration Query Variants
`DiscoveryRequest` SHALL include prefix-listing and label-query variants, and `DiscoveryResponse` SHALL include a multi-target variant carrying the matched targets. These variants SHALL round-trip through the rkyv codec like all other discovery protocol types.

#### Scenario: Prefix query round-trips
- **WHEN** a `DiscoveryRequest` prefix query is encoded and decoded
- **THEN** the query SHALL survive unchanged

#### Scenario: Multi-target response carries every match
- **WHEN** a `DiscoveryResponse` multi-target variant carrying N targets is encoded and decoded
- **THEN** all N targets SHALL survive in order

### Requirement: HostQueueCreate Serving Tenant
`HostcallRequest::HostQueueCreate` SHALL carry an optional serving tenant, mirroring `AllocRegion`'s principal-provenance field, and SHALL round-trip through the rkyv codec unchanged.

#### Scenario: Serving tenant round-trips
- **WHEN** a `HostQueueCreate` request with `serving_tenant: Some("acme")` is encoded and decoded
- **THEN** the decoded request's serving tenant SHALL equal the original

### Requirement: Strict FlatBuffers Wire Decode
The FlatBuffers codec for discovery types SHALL decode strictly: an unknown resource class segment, an unknown request/response variant tag, a `Register` without a target, or a `Found` without a target SHALL fail the decode with an error rather than silently coercing to a default. Both in-fabric endpoints emit the closed vocabularies, so strictness never rejects legitimate traffic.

#### Scenario: Unknown class segment fails decode
- **WHEN** a wire resource target carries a class segment outside the closed `ResourceClass` vocabulary
- **THEN** decoding SHALL return an error

#### Scenario: Unknown variant tag fails decode
- **WHEN** a wire discovery request or response carries a variant tag outside the known set
- **THEN** decoding SHALL return an error
