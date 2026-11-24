# Spec Delta

## ADDED Requirements

### Requirement: Traceability Is Adjacent To Asserting Evidence
A scenario annotation SHALL be attached to the test that directly asserts the scenario or to a reviewed waiver; aggregate annotation blocks and unrelated substrate tests SHALL NOT count as end-to-end evidence.

#### Scenario: Annotation is separated from evidence
- **WHEN** a scenario is named only in a trailing declaration block or beside a test that does not assert its behaviour
- **THEN** semantic review or the enhanced checker SHALL reject that association and report the scenario as uncovered

#### Scenario: Current uncovered scenarios block the gate
- **WHEN** any main-spec scenario lacks adjacent evidence or a reviewed waiver
- **THEN** the required gate SHALL fail and documentation SHALL NOT claim complete traceability

### Requirement: External Protocol Paths Have Real End-To-End Evidence
HTTP and CLI requirements SHALL be exercised through their real external process and WASM-guest paths rather than inferred from codecs or hostcall substrate tests.

#### Scenario: External HTTPS client reaches application guest
- **WHEN** an independent HTTPS client sends a request through the connector to a real application guest
- **THEN** the test SHALL assert the complete response and relevant routing, streaming, and failure behaviour

#### Scenario: CLI subprocess uses real bridge
- **WHEN** the `sel` binary runs against the real connector, bridge, and control-plane guests
- **THEN** the test SHALL assert its wire request, rendered output, and exit status

### Requirement: Crash Injection Stops Durable Components Mid-Write
Crash-consistency tests SHALL interrupt actual store, identity, and control-plane writes at declared persistence boundaries and verify replay after restart; byte-prefix format tests SHALL be labelled separately.

#### Scenario: Store stops between append and manifest publication
- **WHEN** fault injection interrupts a store after blob append but before manifest commit
- **THEN** restart SHALL expose only the complete acknowledged state and recovery SHALL NOT publish a partial blob

#### Scenario: Identity or control plane stops during policy write
- **WHEN** a durable policy guest is interrupted around append, sync, or projection
- **THEN** restart SHALL reconcile to a state consistent with the acknowledgements already returned

### Requirement: Verification Tools And Artifact Paths Are Reproducible
The gate SHALL use pinned toolchain and utility versions and SHALL resolve Cargo artifacts from the configured target directory in both native and container execution.

#### Scenario: Non-default Cargo target directory
- **WHEN** `CARGO_TARGET_DIR` points outside the checkout's `target` directory
- **THEN** every build and artifact guard SHALL inspect the same configured location

#### Scenario: Toolchain update is deliberate
- **WHEN** CI or the local container gate runs without an intentional toolchain-bump change
- **THEN** it SHALL use the checked-in stable, nightly, and utility versions rather than moving tags
