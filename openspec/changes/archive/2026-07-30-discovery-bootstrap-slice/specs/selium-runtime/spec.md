# Spec Delta: selium-runtime

## MODIFIED Requirements

### Requirement: Discovery-Enabled Bootstrap

`selium-runtime` SHALL support `start_discovery` in `RuntimeConfig`,
creating the Tier-1 feed ring and RPC listener, injecting tagged
`WasmValue` entrypoint arguments (feed region id and listener handle into
the discovery guest; listener handle into other guests with empty argument
lists), and gating readiness per guest on `mark_ready()`.

#### Scenario: Discovery wiring uses tagged argument encoding

- **WHEN** the runtime injects discovery arguments into a guest descriptor
- **THEN** `decode_wasm_arguments` decodes every injected value without
  error, for all possible u64 handle values

#### Scenario: Readiness is per-guest

- **WHEN** a bootstrapped guest does not call `mark_ready()` within the
  readiness window
- **THEN** the runtime rolls back the bootstrap and reports
  `ReadinessUnsatisfied` naming that guest

### Requirement: Process Teardown Revocation

When a process exits, the runtime SHALL publish Tier-1 revocation events
for all URIs registered for that process's regions before reclaiming its
resources.

#### Scenario: Exit revokes before reclaim

- **WHEN** a process with allocated regions is stopped
- **THEN** revocation events for its region URIs are published to the
  discovery feed before its shared resources are reclaimed
