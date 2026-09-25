# Tasks: Network Capability URIs

## 1. ABI matching

- [x] 1.1 Add network-endpoint URI parser (scheme/host/port components; canonicalisation: lowercase, strip trailing dot, bracket IPv6, explicit port)
- [x] 1.2 Component-aware matching in `ResourceSelector::matches` for `tcp://`/`udp://` URIs; string prefix retained for other schemes
- [x] 1.3 Flip `is_evaluatable()` for `UriPrefix` gated on a network `ResourceClass` selector in the same grant
- [x] 1.4 Unit tests: exact match, `*.`-wildcard label boundary, port list/`*`, `example.com` vs `example.com.evil.com` rejection, non-network URI prefix unchanged

## 2. Runtime plumbing

- [x] 2.1 Add `require_with_uri` (or extend `require`) to populate `ScopeContext.uri`
- [x] 2.2 `TcpBind`/`TcpConnect`/`UdpBind` handlers construct canonical URIs and evaluate via the URI-populated context
- [x] 2.3 Denial errors include the canonical URI

## 3. Verification

- [x] 3.1 Test: grant `Network + ResourceClass(TcpStream) + UriPrefix("tcp://127.0.0.1:*")` allows loopback connect, denies other hosts
- [x] 3.2 Test: grant `Network + UriPrefix("tcp://10.0.0.5:443")` without a network class selector is rejected at grant-registration time
- [x] 3.3 Test: class-only Network grant retains current allow-all-endpoints behaviour
