# Design: Network Capability URIs

## Context

`ScopeContext.uri` is hardcoded `None` in `require()`
(runtime/process.rs) and `ResourceSelector::UriPrefix` is marked
non-evaluatable (abi) pending exactly this population. See proposal.md
for motivation.

## Goals / Non-Goals

**Goals:**

- Honest, grant-time-checked URI scoping for network endpoints
- Component-aware matching that cannot be fooled by string prefixes

**Non-Goals:**

- CIDR/range selectors (needs post-resolution evaluation; deferred)
- `sel://` URI population for non-network classes (discovery's change)

## Decisions

### Canonical form

```
tcp://93.184.216.34:443      udp://[2001:db8::1]:53
```

- scheme: `tcp` | `udp` (from the hostcall, not the guest string)
- host: lowercase, trailing dot stripped, IPv6 canonicalised and
  bracketed; **literals only** (per `guest-net-sockets`, names never
  reach the hostcall path, so the URI host is always a literal — there is
  no DNS-trust ambiguity in the grant itself)
- port: always explicit

### Component-aware matching

When grant URI and context URI both parse as network endpoints:

| Component | Rule |
| --- | --- |
| scheme | exact match |
| host | exact, or grant host `*.example.com` matches one-or-more leading labels (label boundary only) |
| port | exact, `*`, or list (e.g. `80,443`) |

Since hosts are literals in v1, the `*.` wildcard matters for the
documented extension path (name-based grants if a future change admits
them via the DNS connector) and costs nothing to define now.

Non-network URIs keep plain `starts_with` prefix semantics, so discovery
URIs (`sel://acme/payments/`) are unaffected.

### Evaluatable rule

`UriPrefix` is evaluatable iff the same grant's selectors include
`ResourceClass(TcpListener | TcpStream | UdpSocket)`. Otherwise the grant
is rejected at grant time (invariant 3: never accept a grant that cannot
be enforced). When discovery URIs populate contexts for other classes
later, the conjunction list extends — a deliberate, reviewed act.

### Enforcement point

The runtime, in the hostcall handler, before the kernel touches the OS:
build canonical URI → `ScopeContext { uri: Some(uri), resource_class,
.. }` → `authorises`. Denial returns `PermissionDenied` with the URI in
the message (aids debugging; URIs are literals, no secrecy concern).

### Rejected alternatives

- **String prefix on canonical URIs**: leaves the
  `example.com` vs `example.com.evil.com` label-suffix hole; documenting
  "include the port in your prefix" is a footgun at grant time.
  Component matching is ~15 lines and kills it.
- **CIDR selectors now**: requires post-resolution evaluation and
  rebinding caveats; hosts are literals in v1 so exact/wildcard host
  matching suffices. Explicitly deferred, noted as the extension path.

## Risks / Trade-offs

- Two matching semantics coexist (component-aware for network URIs,
  string prefix for `sel://`) → documented in the ABI; a future
  discovery-URI change may unify them deliberately.
- Canonicalisation mismatches (e.g. IPv6 forms) → canonical form is
  applied to both grant and context before comparison; unit tests pin
  the forms.
