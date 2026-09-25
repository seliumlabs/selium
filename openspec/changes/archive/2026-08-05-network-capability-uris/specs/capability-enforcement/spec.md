## ADDED Requirements

### Requirement: Network Endpoint URIs in Scope Contexts
The runtime SHALL populate `ScopeContext.uri` with a canonical network
endpoint URI (`tcp://<host>:<port>` or `udp://<host>:<port>`) when
evaluating `TcpBind`, `TcpConnect`, and `UdpBind` hostcalls.
Canonicalisation SHALL lowercase the host, strip any trailing dot,
bracket IPv6 literals, and always include the port explicitly.

#### Scenario: Connect evaluated against URI grant
- **WHEN** a guest issues `TcpConnect` to `93.184.216.34:443`
- **THEN** the runtime SHALL evaluate grants against
  `uri = "tcp://93.184.216.34:443"` with
  `resource_class = TcpStream`

#### Scenario: Denial names the URI
- **WHEN** no grant admits the requested endpoint
- **THEN** the hostcall SHALL fail with `PermissionDenied` and the error
  message SHALL include the canonical URI

### Requirement: Component-Aware URI Prefix Matching
When a `ResourceSelector::UriPrefix` grant and the context URI both parse
as network endpoints, matching SHALL compare components: scheme exact;
host exact or `*.`-label-boundary wildcard; port exact, list, or `*`.
Plain string prefix semantics SHALL remain for non-network URIs.

#### Scenario: Label-suffix attack rejected
- **WHEN** a grant carries `UriPrefix("tcp://93.184.216.34:443")`
- **THEN** a context URI of `tcp://93.184.216.34:443` matches, and a
  context URI whose host merely has the grant host as a string prefix
  (e.g. a longer differing literal) SHALL NOT match

#### Scenario: Port wildcard
- **WHEN** a grant carries `UriPrefix("tcp://127.0.0.1:*")`
- **THEN** any loopback port matches and any non-loopback host SHALL NOT
  match

### Requirement: Grant-Time Evaluatable Honesty for UriPrefix
A `CapabilityGrant` containing `ResourceSelector::UriPrefix` SHALL be
accepted at grant-registration time only if the same grant's selectors
include `ResourceClass::TcpListener`, `ResourceClass::TcpStream`, or
`ResourceClass::UdpSocket`. Otherwise registration SHALL fail loudly.

#### Scenario: UriPrefix on non-network class rejected
- **WHEN** a grant is registered with selectors
  `[ResourceClass(DurableLog), UriPrefix("tcp://10.0.0.5:443")]`
- **THEN** registration SHALL fail with an error explaining that
  `UriPrefix` is not evaluatable for that class
