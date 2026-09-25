## Purpose

`Context` is the bootstrap object passed to every Selium guest entrypoint, providing access to shared memory primitives and a pre-connected discovery RPC client for URI resolution, registration, and revocation.

## Requirements

### Requirement: Discovery client in Context
`Context` SHALL provide access to a pre-connected `RpcClient<DiscoveryRequest, DiscoveryResponse>` through a `discovery()` method. The runtime SHALL establish this connection during bootstrap before the entrypoint is called. The `Context` SHALL use `RpcClient` from `selium-guest::io::rpc` rather than implementing the RPC protocol inline.

#### Scenario: Guest resolves a URI
- **WHEN** a guest calls `ctx.discovery().request(DiscoveryRequest::Resolve(uri)).await`
- **THEN** the system SHALL send the request to the discovery service over the pre-established RPC session and return the `DiscoveryResponse`

### Requirement: Context lookup convenience method
`Context` SHALL provide a `lookup(&mut self, uri: &str) -> Result<Option<ResourceTarget>, GuestError>` convenience method that delegates to `self.discovery().request(DiscoveryRequest::Resolve(uri.to_string())).await` and maps the response.

#### Scenario: Guest resolves a known URI via lookup
- **WHEN** a guest calls `ctx.lookup(uri)` with a URI registered in discovery
- **THEN** the method SHALL send a `DiscoveryRequest::Resolve` via the `RpcClient` and return `Ok(Some(ResourceTarget))` on `DiscoveryResponse::Found`

#### Scenario: Guest resolves an unknown URI via lookup
- **WHEN** a guest calls `ctx.lookup(uri)` with an unregistered URI
- **THEN** the method SHALL return `Ok(None)` on `DiscoveryResponse::NotFound`

#### Scenario: Discovery service disconnected during lookup
- **WHEN** a guest calls `ctx.lookup(uri)` and the discovery service has disconnected
- **THEN** the method SHALL return `Err(GuestError::Host(...))` with a message indicating the discovery service disconnected

### Requirement: Context register convenience method
`Context` SHALL provide a `register(&mut self, uri: &str, target: ResourceTarget) -> Result<(), GuestError>` convenience method that delegates to `self.discovery().request(DiscoveryRequest::Register { uri: uri.to_string(), target }).await` and maps the response.

#### Scenario: Guest registers a URI via Context
- **WHEN** a guest calls `ctx.register("sel://tenant/logs/app", target).await`
- **THEN** the method SHALL send `DiscoveryRequest::Register` via the RPC client and return `Ok(())` on `DiscoveryResponse::Registered`

#### Scenario: Registration rejected (Forbidden)
- **WHEN** a guest calls `ctx.register(uri, target).await` and the discovery service returns `DiscoveryResponse::Forbidden`
- **THEN** the method SHALL return `Err(GuestError::Host("registration forbidden"))`

### Requirement: Context revoke convenience method
`Context` SHALL provide a `revoke(&mut self, uri: &str) -> Result<(), GuestError>` convenience method that delegates to `self.discovery().request(DiscoveryRequest::Revoke { uri: uri.to_string() }).await` and maps the response.

#### Scenario: Guest revokes a URI via Context
- **WHEN** a guest calls `ctx.revoke("sel://tenant/logs/app").await`
- **THEN** the method SHALL send `DiscoveryRequest::Revoke` via the RPC client and return `Ok(())` on `DiscoveryResponse::Revoked`

### Requirement: Serve-based route registration
`Context` SHALL provide a `serve` method that registers a named route from a resource the guest created itself. The route SHALL derive from a path (for example `["bridge"]`) plus the guest's tenant, producing both the internal path (`sel://<tenant>/bridge`) and the wire name (`bridge.<tenant>`, or `bridge.<owned-domain>` when the domain table maps the tenant). The method SHALL optionally accept a root-service flag for apex aliasing.

#### Scenario: Guest registers its own route
- **WHEN** a guest calls `ctx.serve(Serve { path: ["bridge"], target, default: false }).await`
- **THEN** discovery SHALL store the route resolvable as `sel://<tenant>/bridge` and `bridge.<tenant>`

#### Scenario: Route registration derives wire name from the domain table
- **WHEN** the guest's tenant owns a registered domain and serves a path
- **THEN** the wire name under that domain SHALL also resolve to the route

### Requirement: Root registration capability-gated
Registration in the root/system tenant SHALL be permitted only when the guest holds the corresponding registration capability, replacing the runtime's special-case well-known URI provisioning.

#### Scenario: Root registration requires capability
- **WHEN** a guest without the system-registration capability attempts to register in the root tenant
- **THEN** the request SHALL be forbidden

#### Scenario: Capability-holding guest registers in root
- **WHEN** a guest holding the system-registration capability registers in the root tenant
- **THEN** the registration SHALL be accepted
