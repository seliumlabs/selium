## ADDED Requirements

### Requirement: HostCaller Exposes Caller Identity
`HostCaller` SHALL carry a `consumer_id: u64` field identifying the calling WASM instance. This field SHALL be set when the runtime constructs the `HostCaller` during host function dispatch. Host function implementations SHALL use `caller.consumer_id()` to authorise operations scoped to the calling guest.

#### Scenario: HostCaller provides caller identity
- **WHEN** a host function is invoked by a guest
- **THEN** `HostCaller::consumer_id()` SHALL return the unique identifier of the calling guest instance

#### Scenario: Slot write hostcall validates against caller identity
- **WHEN** the `write_slot` hostcall handler reads `caller.consumer_id()`
- **THEN** it SHALL use this value to look up slot ownership in `SlotManager` and reject writes to slots not owned by the caller

### Requirement: Instance Carries Consumer ID
`Instance` SHALL carry a `consumer_id: u64` field, set by the runtime at guest instantiation. This field SHALL be passed through to `HostCaller` during `call_cloned_host_func`.

#### Scenario: Runtime sets consumer_id on guest instantiation
- **WHEN** `selium-runtime` instantiates a WASM guest module
- **THEN** it SHALL assign a unique `consumer_id` to the `Instance` before registering host functions

#### Scenario: HostCaller constructed with instance's consumer_id
- **WHEN** `Instance::call_cloned_host_func` constructs a `HostCaller`
- **THEN** it SHALL pass `self.consumer_id` to `HostCaller::new`
