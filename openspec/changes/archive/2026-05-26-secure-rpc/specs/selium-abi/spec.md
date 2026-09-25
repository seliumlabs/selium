## MODIFIED Requirements

### Requirement: Stable Hostcall Contracts
`selium-abi` SHALL define stable identifiers, request payloads, response payloads, and completion states for hostcalls shared between host and guest crates.

#### Scenario: HostQueueSend hostcall
- **WHEN** a guest invokes `HostQueueSend { handle, value }`
- **THEN** the runtime SHALL validate the guest's capability to reach the target service, enqueue the value in the service's connection queue, and return success or a capability error

#### Scenario: HostQueueRecv hostcall
- **WHEN** a guest invokes `HostQueueRecv { handle }`
- **THEN** the runtime SHALL return the next pending connection entry containing the client's `ProcessId` and the enqueued value, or block if no connection is pending

#### Scenario: Pending hostcall completion
- **WHEN** a guest invokes an asynchronous hostcall that cannot complete immediately
- **THEN** the ABI SHALL return a pending completion state that both runtime and guest SDK interpret consistently

## ADDED Requirements

### Requirement: Connection queue hostcall output
The `HostcallOutput` enum SHALL include a `ConnectionInfo` variant containing `client_process_id: ProcessId` and `value: u64` for use by `HostQueueRecv`.

#### Scenario: Server receives connection info
- **WHEN** a server guest polls a `HostQueueRecv` operation that has completed
- **THEN** the completion state SHALL contain a `ConnectionInfo` with the connecting client's process ID and the shared resource identifier