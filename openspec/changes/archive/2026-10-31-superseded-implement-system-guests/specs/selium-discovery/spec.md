## ADDED Requirements

### Requirement: Discovery Guest Crate
The system SHALL provide a `selium-discovery` guest crate that can be built as a `wasm32-unknown-unknown` system guest and bootstrapped through `selium-runtime` configuration.

#### Scenario: Discovery guest bootstraps from descriptor
- **WHEN** `selium-runtime` receives a `SystemGuestDescriptor` for `selium-discovery`
- **THEN** it SHALL start the discovery guest using the descriptor's module, zero-argument entrypoint, grants, dependencies, and readiness condition

### Requirement: URI Registration Store
The discovery guest SHALL maintain a durable mapping from Selium URIs to the host-visible resources and interfaces they represent.

#### Scenario: Resource registered
- **WHEN** a platform resource or guest interface is registered with a Selium URI
- **THEN** the discovery guest SHALL persist the mapping in its registration store

#### Scenario: Running guest persists registration
- **WHEN** a registration request reaches discovery through its configured resource
- **THEN** the running guest entrypoint SHALL write the mapping to a durable host resource before acknowledging it
- **AND** native-only map insertion SHALL NOT satisfy persistence

### Requirement: Exact URI Resolution
The discovery guest SHALL resolve an exact Selium URI to the corresponding host-visible resource or interface.

#### Scenario: Exact URI resolved
- **WHEN** a guest requests resolution for a registered exact URI
- **THEN** the discovery guest SHALL return the mapped host and resource information

### Requirement: Prefix-Based Discovery
The discovery guest SHALL support prefix-based discovery for URI hierarchies.

#### Scenario: Prefix query executed
- **WHEN** a guest requests discovery for a URI prefix
- **THEN** the discovery guest SHALL return the matching registered resources or interfaces for that prefix

### Requirement: Explicit Discovery Interfaces
The discovery guest SHALL expose registration and resolution behaviour through explicitly defined `selium-io` state/topics or network request exchanges.

#### Scenario: Guest performs discovery query
- **WHEN** another guest performs a discovery query through a supported interface
- **THEN** the discovery guest SHALL return the matching discovery result through that interface without relying on an unspecified messaging layer

#### Scenario: Discovery interface is concrete
- **WHEN** discovery exposes registration or resolution
- **THEN** it SHALL use named request-exchange, topic, live-table, or durable-log resources available through `selium-guest`/`selium-io`

### Requirement: Interface Metadata Visibility
The discovery guest SHALL retain and return guest-facing interface metadata needed for callers to discover how to interact with registered resources.

#### Scenario: Interface metadata returned
- **WHEN** a caller resolves a registered guest-facing interface
- **THEN** the discovery guest SHALL return the associated interface metadata along with the resource mapping
