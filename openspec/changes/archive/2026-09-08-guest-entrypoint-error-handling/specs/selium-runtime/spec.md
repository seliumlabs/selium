## MODIFIED Requirements

### Requirement: Discovery-Enabled Bootstrap

`selium-runtime` SHALL support `start_discovery` in `RuntimeConfig`,
creating the Tier-1 feed ring and RPC listener, injecting tagged `WasmValue`
entrypoint arguments (feed region id and listener handle into the discovery
guest; listener handle into other guests with empty argument lists), and
gating readiness per guest on `mark_ready()`.

#### Scenario: Discovery wiring uses tagged argument encoding

- **WHEN** the runtime injects discovery arguments into a guest descriptor
- **THEN** `decode_wasm_arguments` decodes every injected value without error, for all possible u64 handle values

#### Scenario: Readiness is per-guest

- **WHEN** a bootstrapped guest does not call `mark_ready()` within the readiness window
- **THEN** the runtime rolls back the bootstrap and reports `ReadinessUnsatisfied` naming that guest

#### Scenario: Application guest receives discovery handle

- **WHEN** the runtime bootstraps an application guest
- **THEN** the guest's entrypoint SHALL receive the discovery `shared_id`
  as the first entrypoint argument slot, and the entrypoint macro's
  generated glue SHALL construct the `Context` from it for
  `Context`-leading entrypoints (guests do not call `Context::from_raw`
  themselves)

### Requirement: Discovery handle passed to guest entrypoints

The runtime SHALL prepend the discovery host queue `shared_id` as the
first entrypoint argument slot (existing behaviour, unchanged). For
`Context`-leading entrypoints, the `#[entrypoint]` macro constructs the
`Context` from that slot via `Context::from_raw`; guests do not call
`Context::from_raw` in their own code. The runtime's own authoritative
discovery RPC session SHALL be separate from the guest-facing discovery
queue.

#### Scenario: Application guest receives discovery handle (unchanged)

- **WHEN** the runtime bootstraps an application guest
- **THEN** the guest's entrypoint SHALL receive the discovery `shared_id`
  as the first argument slot
- **AND** the entrypoint macro SHALL construct the leading `Context` from
  that slot for `Context`-declaring entrypoints