## ADDED Requirements

### Requirement: WaitRegister Hostcall
The ABI SHALL define `HostcallRequest::WaitRegister { region_id,
generation }`, rkyv-encoded like all hostcall requests. The request
registers the calling process's interest in a generation advance of the
identified shared region; the guest task to wake is carried by the
envelope's existing `task_id` field.

#### Scenario: Round-trip encoding
- **WHEN** a `WaitRegister` request is encoded and decoded
- **THEN** `region_id` and `generation` SHALL survive unchanged

#### Scenario: Wake routed via envelope task
- **WHEN** the runtime observes a host-side generation advance past a
  registered generation for that region
- **THEN** it SHALL wake the task identified by the registering
  envelope's `task_id`, and SHALL NOT wake tasks of any other process

#### Scenario: Unattached region rejected
- **WHEN** a process issues `WaitRegister` for a region it has not
  attached
- **THEN** the hostcall SHALL fail loudly
