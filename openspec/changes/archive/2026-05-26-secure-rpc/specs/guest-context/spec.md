## ADDED Requirements

### Requirement: Entrypoint Context injection
The `#[entrypoint]` macro SHALL accept guest functions that take a `Context` parameter. The runtime SHALL construct and inject the `Context` during guest bootstrap before calling the entrypoint function.

#### Scenario: Entrypoint receives Context
- **WHEN** a guest defines `#[entrypoint] async fn my_guest(ctx: Context) { ... }`
- **THEN** the runtime SHALL decode the `Context` from the bootstrap payload and pass it to the entrypoint function

#### Scenario: Entrypoint without Context (backwards compatibility)
- **WHEN** a guest defines `#[entrypoint] async fn my_guest() { ... }` without a `Context` parameter
- **THEN** the system SHALL still support this form, providing an empty or default `Context`

### Requirement: Discovery client in Context
`Context` SHALL provide access to a pre-connected `RpcClient<DiscoveryRequest, DiscoveryResponse>` through a `discovery()` method. The runtime SHALL establish this connection during bootstrap before the entrypoint is called.

#### Scenario: Guest resolves a URI
- **WHEN** a guest calls `ctx.discovery().request(DiscoveryRequest::Resolve(uri)).await`
- **THEN** the system SHALL send the request to the discovery service over the pre-established RPC session and return the `DiscoveryResponse`

### Requirement: Resource sender handle in Context
`Context` SHALL contain a `ResourceSender` handle that the guest can use to establish new RPC connections to other services. The runtime SHALL provide this handle during bootstrap.

#### Scenario: Guest connects to a custom service
- **WHEN** a guest calls `ResourceSender::attach(handle)` followed by `sender.send(shared_id).await`
- **THEN** the system SHALL send the session's `shared_id` to the target service's connection queue through the host