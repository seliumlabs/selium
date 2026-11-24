# Spec Delta

## MODIFIED Requirements

### Requirement: Global RegionProvider Installation
`selium-memory` SHALL support a process-global provider for a single WASM guest context. Native runtime-owned I/O SHALL instead receive an explicit provider or context so independent runtimes cannot share allocations accidentally. Installing a guest provider SHALL fail loudly if a different provider is already installed.

#### Scenario: Provider installed before I/O operations
- **WHEN** a WASM guest installs its hostcall-backed provider during early initialisation
- **THEN** subsequent channel creation SHALL allocate through that provider

#### Scenario: No provider installed
- **WHEN** an I/O operation requiring the ambient guest provider is attempted before installation
- **THEN** the operation SHALL return an error indicating no provider is configured

#### Scenario: Independent native runtimes
- **WHEN** two runtimes backed by different kernels coexist in one host process and each creates discovery channels
- **THEN** every allocation and attachment SHALL use the owning runtime's provider with no cross-runtime resource ids

#### Scenario: Conflicting guest provider installation
- **WHEN** guest initialisation attempts to replace an already installed distinct provider
- **THEN** installation SHALL return an error rather than silently retaining or replacing the wrong context
