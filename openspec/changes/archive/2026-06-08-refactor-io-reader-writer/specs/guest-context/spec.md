## MODIFIED Requirements

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

## REMOVED Requirements

### Requirement: Inline RPC implementation in Context
**Reason**: `Context` previously implemented the RPC framing protocol inline (frame header encoding/decoding, generation counter polling, writer count checks) because `selium-rpc` depended on `selium-guest`, creating a circular dependency. With `RpcClient` now residing in `selium-guest::io::rpc`, the inline implementation is unnecessary.
**Migration**: No guest code changes required. `Context::lookup` retains the same signature and behavior.
