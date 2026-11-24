# Spec Delta

## MODIFIED Requirements

### Requirement: Host Keyring Persistence

`selium-runtime` SHALL persist the root public certificate, online intermediate key material, and per-tenant CA keys needed for restart. The root private key SHALL NOT be persisted in the ordinary online state directory. Existing tenant CAs and certificate chains SHALL remain valid across restart.

#### Scenario: First boot generates and persists

- **WHEN** the host boots with an empty state directory
- **THEN** it SHALL establish the hierarchy, persist only online operational secrets and public root material, and reuse that material on subsequent boots

#### Scenario: Restart preserves the hierarchy

- **WHEN** the host restarts and mints for a previously onboarded tenant
- **THEN** the issued certificate SHALL chain to the recorded tenant CA without requiring the root private key

#### Scenario: Legacy online root key is migrated

- **WHEN** a state directory from an earlier version contains `root.key.der`
- **THEN** migration SHALL remove that online private key after validating the hierarchy and SHALL report any removal failure as a security-relevant boot error

## ADDED Requirements

### Requirement: Failed Resource Operations Roll Back Reservations
Quota and ownership changes for a resource operation SHALL commit only when the operation succeeds; failure SHALL restore the prior accounting and ownership state.

#### Scenario: Region allocation fails after reservation
- **WHEN** shared-memory quota reservation succeeds but region allocation fails
- **THEN** the reserved bytes SHALL be released and no ownership record SHALL remain

#### Scenario: Append or queue send fails
- **WHEN** a volume append or queue send fails after charging quota
- **THEN** its reservation SHALL be released and a later retry SHALL observe the original available headroom

### Requirement: State Preparation Completes Before Its Success Marker
State-root preparation SHALL publish its completed marker only after rooting volumes, reconstructing usage, and loading key material all succeed; a failed attempt SHALL remain retryable.

#### Scenario: Initial key loading fails
- **WHEN** any state preparation step returns an error
- **THEN** bootstrap SHALL fail and a corrected retry SHALL execute every incomplete step rather than return false success

### Requirement: Runtime Shutdown Reclaims Background Execution
A runtime SHALL provide bounded shutdown that stops its poller, timer, metering, and other owned background activity, joins or accounts for their termination, and releases runtime-held state.

#### Scenario: Runtime is dropped or shut down
- **WHEN** an embedder shuts down a runtime after bootstrapping guests
- **THEN** no runtime-owned thread SHALL retain the runtime indefinitely and repeated create/shutdown cycles SHALL leave bounded thread and resource counts

### Requirement: Hostcall Dispatch Is Total Over Untrusted Input
Every guest-supplied hostcall input SHALL produce a valid completion or structured failure without host panic, unbounded allocation, or process abort.

#### Scenario: Malformed extreme inputs
- **WHEN** hostile lengths, counts, identifiers, and durations are exercised through direct and fuzzed hostcalls
- **THEN** the runtime SHALL remain available and reject invalid requests within documented resource bounds
