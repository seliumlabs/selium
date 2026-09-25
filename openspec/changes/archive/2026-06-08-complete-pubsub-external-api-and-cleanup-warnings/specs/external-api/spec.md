## ADDED Requirements

### Requirement: Text-Protocol Request Parsing
`selium-external-api` SHALL parse whitespace-delimited text requests into `UserIntent` values. The protocol grammar SHALL support the commands `deploy`, `start`, `stop`, `scale`, and `resolve`, each with positional arguments.

#### Scenario: Parse a deploy request
- **WHEN** the API receives `"deploy my-workload 3"`
- **THEN** `parse_intent` SHALL return `UserIntent::Deploy { workload_id: "my-workload", replicas: 3 }`

#### Scenario: Parse a resolve request
- **WHEN** the API receives `"resolve selium://my-service"`
- **THEN** `parse_intent` SHALL return `UserIntent::Resolve { uri: "selium://my-service" }`

#### Scenario: Parse an unknown command
- **WHEN** the API receives `"unknown-command arg1"`
- **THEN** `parse_intent` SHALL return `Err(ApiError::UnknownCommand("unknown-command"))`

#### Scenario: Parse an empty request
- **WHEN** the API receives an empty string or whitespace-only request
- **THEN** `parse_intent` SHALL return `Err(ApiError::EmptyRequest)`

#### Scenario: Parse with missing required argument
- **WHEN** the API receives `"deploy"` without a workload_id
- **THEN** `parse_intent` SHALL return `Err(ApiError::MissingArgument("workload_id"))`

### Requirement: Intent Decomposition into Delegated Interactions
`selium-external-api` SHALL decompose `UserIntent` values into ordered lists of `DelegatedInteraction` steps. Each step SHALL correspond to an RPC call to a system guest (discovery, scheduler).

#### Scenario: Deploy intent decomposes to resolve + place
- **WHEN** `decompose_intent` receives `UserIntent::Deploy { workload_id: "w", replicas: 3 }`
- **THEN** it SHALL return `[DiscoveryResolve { uri: "w" }, SchedulerPlace { workload_id: "w", replicas: 3 }]`

#### Scenario: Stop intent decomposes to scheduler stop only
- **WHEN** `decompose_intent` receives `UserIntent::Stop { workload_id: "w" }`
- **THEN** it SHALL return `[SchedulerStop { workload_id: "w" }]`

#### Scenario: Scale intent decomposes to scheduler scale only
- **WHEN** `decompose_intent` receives `UserIntent::Scale { workload_id: "w", replicas: 5 }`
- **THEN** it SHALL return `[SchedulerScale { workload_id: "w", replicas: 5 }]`

#### Scenario: Resolve intent decomposes to discovery resolve only
- **WHEN** `decompose_intent` receives `UserIntent::Resolve { uri: "u" }`
- **THEN** it SHALL return `[DiscoveryResolve { uri: "u" }]`

### Requirement: Delegation Dispatch via RPC
`selium-external-api` SHALL dispatch each `DelegatedInteraction` to the appropriate system guest over RPC. Discovery interactions SHALL be sent to the discovery guest. Scheduler interactions SHALL be sent to the scheduler guest.

#### Scenario: Dispatch discovery resolve
- **WHEN** the API processes a `DiscoveryResolve { uri }` interaction
- **THEN** it SHALL call `discovery_client.request(DiscoveryRequest::Resolve(uri))` and await the response

#### Scenario: Dispatch scheduler place
- **WHEN** the API processes a `SchedulerPlace { workload_id, replicas }` interaction
- **THEN** it SHALL call `scheduler_client.request(SchedulerRequest::Place { workload_id, replicas })` and await the response

#### Scenario: Dispatch errors surface as delegation failures
- **WHEN** an RPC call to discovery or scheduler fails
- **THEN** the API SHALL return `ApiError::DelegationFailed { step, context }` describing which step failed

### Requirement: Inbound Network Bridge Interface
`selium-external-api` SHALL receive client connections through a runtime-managed inbound network bridge. Each connection SHALL be presented to the guest as a pair of shared-memory ring buffers (inbound for reading client requests, outbound for writing responses), following the same `TcpStream` ring buffer layout.

#### Scenario: API guest receives a connection
- **WHEN** the runtime's inbound bridge accepts an external TCP connection
- **THEN** the runtime SHALL spawn (or route to) the external API guest instance and provide the connection's ring buffers via the guest's bootstrap context

#### Scenario: API guest reads a request and writes a response
- **WHEN** the API guest reads a complete request from the inbound ring buffer and processes it
- **THEN** it SHALL write the `ClientFeedback` response to the outbound ring buffer

### Requirement: ApiContext Bootstrap
`selium-external-api` SHALL receive an `ApiContext` during bootstrap containing pre-connected RPC clients for discovery and scheduler, plus the inbound bridge handle for accepting client connections.

#### Scenario: ApiContext provides discovery client
- **WHEN** the API guest entrypoint receives its `ApiContext`
- **THEN** `ctx.discovery()` SHALL return an `RpcClient<DiscoveryRequest, DiscoveryResponse>` ready for requests

#### Scenario: ApiContext provides scheduler client
- **WHEN** the API guest entrypoint receives its `ApiContext`
- **THEN** `ctx.scheduler()` SHALL return an `RpcClient<SchedulerRequest, SchedulerResponse>` ready for requests

### Requirement: Client Feedback Response
After processing a request, `selium-external-api` SHALL return a `ClientFeedback` struct containing an acceptance flag, a human-readable message, and the list of delegated interactions that were dispatched.

#### Scenario: Successful request returns feedback
- **WHEN** `accept_request` successfully parses and dispatches a request
- **THEN** it SHALL return `ClientFeedback { accepted: true, message: "request accepted", delegated: [...] }`

#### Scenario: Failed request returns feedback
- **WHEN** `accept_request` encounters a parse error
- **THEN** it SHALL return `ClientFeedback { accepted: false, message: "<error description>", delegated: [] }`
