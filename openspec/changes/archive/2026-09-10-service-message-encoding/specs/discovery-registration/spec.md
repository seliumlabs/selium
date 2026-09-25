## ADDED Requirements

### Requirement: FlatBuffers Tier-1 Discovery Feed

The runtime→discovery (Tier-1) event feed SHALL carry FlatBuffers-encoded `DiscoveryRequest` values. The runtime SHALL publish typed `DiscoveryRequest` values (encoded via `FlatMsg`) instead of raw rkyv bytes, and the discovery guest SHALL subscribe to and decode typed `DiscoveryRequest` values via `FlatMsg`. The feed representation SHALL cover every Tier-1 operation — `Register` (including an optional owner), `Revoke`, `RevokeByOwner`, and `SeedDomain` — without lossy collapse onto an unrelated variant.

#### Scenario: Process-node registration round-trips over the feed

- **WHEN** the runtime publishes a `DiscoveryRequest::Register` for a spawned process
- **THEN** the discovery guest SHALL decode the same registration, preserving `uri`, `target`, and `owner`

#### Scenario: Revoke-by-owner round-trips over the feed

- **WHEN** the runtime publishes `DiscoveryRequest::RevokeByOwner { process_id }`
- **THEN** the feed SHALL carry the operation with its `process_id` intact
- **AND** the discovery guest SHALL decode it as `RevokeByOwner` rather than another variant

#### Scenario: Seed-domain round-trips over the feed

- **WHEN** the runtime publishes `DiscoveryRequest::SeedDomain { domain, tenant }`
- **THEN** the feed SHALL carry both `domain` and `tenant` intact to the discovery guest

#### Scenario: Feed is not rkyv-encoded

- **WHEN** the runtime publishes to or the discovery guest reads from the Tier-1 feed
- **THEN** neither side SHALL use `selium_abi::encode_rkyv` or `decode_rkyv` for the feed payload
