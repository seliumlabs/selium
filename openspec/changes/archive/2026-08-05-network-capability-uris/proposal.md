# Proposal: Network Capability URIs

## Why

The runtime's network hostcall handlers check `Capability::Network` and
the resource class, but pass `None` for the resource selector context: a
guest holding a Network grant can connect to or bind **anything**
reachable from the host. That violates invariant 3 in spirit — the grant
is broader than anything the system can honestly scope. Meanwhile the
selector algebra already anticipates the fix: `ScopeContext.uri` exists
and is hardcoded to `None` in `require()` ("populated when known"), and
`ResourceSelector::UriPrefix` exists but `is_evaluatable()` returns
`false` for it ("rejected until resource URIs populate scope contexts").

This change connects the two halves: network hostcalls populate the
context URI, and `UriPrefix` matching on network endpoints becomes
component-aware so grants like `tcp://93.184.216.0/24:*`… (future) or
`tcp://10.0.0.5:443` can be enforced honestly, today.

## What Changes

- **Canonical network URIs**: network hostcall addresses are
  canonicalised to `tcp://<host>:<port>` / `udp://<host>:<port>`
  (lowercase, no trailing dot, explicit port, bracketed IPv6) and placed
  in `ScopeContext.uri` by the runtime before grant evaluation.
- **Component-aware `UriPrefix` matching**: when both grant and context
  URI parse as network endpoints (`tcp://`, `udp://`), matching compares
  components — scheme exact; host exact or `*.`-label-boundary wildcard;
  port exact, list, or `*`. String `starts_with` semantics remain for
  non-network URIs (e.g. `sel://` discovery URIs). The
  `tcp://example.com` matching `tcp://example.com.evil.com` footgun is
  eliminated by construction.
- **Grant-time honesty**: `is_evaluatable()` flips to `true` for
  `UriPrefix` **only** when the grant also carries a network
  `ResourceClass` selector (`TcpListener`/`TcpStream`/`UdpSocket`) — the
  only contexts that populate `uri` today. UriPrefix grants without a
  network class selector remain rejected loudly at grant time.
- Grant-less behaviour unchanged: a Network grant with only a
  `ResourceClass` selector still allows all endpoints of that class
  (current behaviour); adding a `UriPrefix` narrows it.

## Capabilities

### New Capabilities

(None.)

### Modified Capabilities

- `capability-enforcement`: `ScopeContext.uri` populated for network
  hostcalls; component-aware `UriPrefix` matching; grant-time
  evaluatable rule for `UriPrefix`

No new `Capability` variants — this narrows how existing `Network`
grants are scoped and enforced.

## Impact

- `selium-abi`: component-aware matching in `ResourceSelector::matches`;
  `is_evaluatable` rule change.
- `selium-runtime`: `require` gains a URI (or a `require_with_uri`
  variant); the three network hostcall handlers construct canonical URIs.
- Specs: MODIFIED `capability-enforcement`.
- Out of scope (documented in design): CIDR/range selectors evaluated
  post-resolution, and `sel://` discovery-URI population for non-network
  classes.
