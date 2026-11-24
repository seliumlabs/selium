# Spec Delta

## ADDED Requirements

### Requirement: Recovery Precedes Every First Blob Mutation
A blob store's first mutation after process start SHALL recover existing blobs and manifests before changing in-memory or durable state.

#### Scenario: Put is the first operation after restart
- **WHEN** a store restarts with existing blob volumes and its first request is a new `put`
- **THEN** recovery SHALL complete before the put and all pre-existing blobs and manifests SHALL remain resolvable afterwards

### Requirement: Recovered Blob Content Matches Its Identity
Recovery SHALL verify that each recovered blob's complete bytes match its content-derived id and length before exposing it or resolving slices.

#### Scenario: Torn blob append
- **WHEN** a blob volume contains a partial or uncommitted append after interruption
- **THEN** recovery SHALL NOT expose the blob id or a slice to those bytes and SHALL preserve prior valid state

#### Scenario: Existing content is put again
- **WHEN** a caller puts bytes whose blob id already exists
- **THEN** the store SHALL validate the existing content and acknowledge it without appending a second copy or changing existing slice offsets
