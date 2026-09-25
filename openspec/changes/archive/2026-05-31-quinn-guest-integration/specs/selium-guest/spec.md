## ADDED Requirements

### Requirement: Optional Quinn Feature
`selium-guest` SHALL define an optional `quinn` feature that enables the Quinn trait implementations within `net/udp.rs`.

#### Scenario: Quinn feature enabled
- **WHEN** the `quinn` feature is enabled in a guest's `Cargo.toml`
- **THEN** the `quinn` crate dependency SHALL be available and `mod quinn_impl` within `net/udp.rs` SHALL be compiled

#### Scenario: Quinn feature disabled
- **WHEN** the `quinn` feature is not enabled
- **THEN** no Quinn code SHALL be compiled and the guest crate SHALL NOT depend on `quinn`

### Requirement: Feature-Gated Public API
The Quinn integration types (`SeliumQuinnRuntime`, conversion method) SHALL be publicly accessible under the `quinn` feature gate, following the same pattern as the Axum integration.

#### Scenario: Guest accesses Quinn runtime type
- **WHEN** a guest with `quinn` feature enabled references `selium_guest::net::udp::SeliumQuinnRuntime`
- **THEN** the type SHALL be resolvable

### Requirement: Internal Unsafe Send+Sync for Channel State
The inner channel state required by Quinn's `Send + Sync` bounds SHALL use `unsafe impl Send` and `unsafe impl Sync` with documented safety invariants.

#### Scenario: Quinn socket wrapper is Send + Sync
- **WHEN** the compiler checks trait bounds for `QuinnUdpSocket`
- **THEN** it SHALL satisfy `Send + Sync + 'static` as required by `quinn::AsyncUdpSocket`
