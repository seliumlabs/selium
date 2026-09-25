## ADDED Requirements

### Requirement: Guest Entrypoint Macro
`selium-guest-macros` SHALL provide a macro that generates the ABI glue needed to expose a guest entrypoint through `selium-abi` and `selium-guest`.

#### Scenario: Macro-generated entrypoint glue
- **WHEN** a guest author annotates a supported async entrypoint with the provided macro
- **THEN** the macro SHALL generate the required ABI-compatible entry glue

### Requirement: Generated Pattern Metadata
`selium-guest-macros` SHALL generate metadata for guest-declared messaging interfaces so that discovery and binding layers can reason about them consistently.

#### Scenario: Messaging interface metadata emitted
- **WHEN** a guest declares a messaging interface using the macro layer
- **THEN** the macro SHALL emit metadata describing the interface in a form consumable by the guest SDK and runtime tooling

### Requirement: Bootstrap-Aware Macro Integration
`selium-guest-macros` SHALL interoperate with the guest SDK so generated entrypoints and messaging metadata can participate in runtime bootstrap and tracing setup.

#### Scenario: Runtime bootstraps macro-based guest
- **WHEN** the runtime starts a guest built with the macro layer
- **THEN** the generated glue SHALL remain compatible with runtime bootstrap expectations and guest tracing setup
