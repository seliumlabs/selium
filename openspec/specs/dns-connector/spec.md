## Purpose

Define the DNS connector: an egress-only system guest that performs real
DNS resolution on behalf of other guests, exposing it as a typed,
capability-gated RPC so that no ambient name resolution exists in host
or runtime paths.

## Requirements

### Requirement: Typed Egress Resolution
Guests SHALL resolve names by sending a schema-encoded `DnsQuery` to the
connector's channel and receiving a typed `DnsResponse`. The runtime and
kernel SHALL NOT perform name resolution on behalf of any guest.

#### Scenario: Successful resolution
- **WHEN** a granted guest sends `DnsQuery` for a name with an A record
- **THEN** the connector SHALL query the upstream resolver over UDP and
  return a `DnsResponse` carrying the resolved addresses

#### Scenario: Resolution is grant-gated
- **WHEN** a guest without a grant for the connector's channel attempts
  to send a query
- **THEN** the attach or send SHALL be denied, and no DNS traffic SHALL
  leave the host

### Requirement: Reply Correlation
The connector SHALL correlate upstream replies to in-flight queries by
transaction id and source address, and SHALL deliver each typed response
with the requesting frame's tag. Replies with unknown transaction ids
SHALL be dropped.

#### Scenario: Concurrent queries do not cross
- **WHEN** two queries are in flight and the upstream replies arrive out
  of order
- **THEN** each response SHALL be delivered to its originating requester
  with the correct tag

### Requirement: Honest Failure Outcomes
Upstream timeout, NXDOMAIN, truncated responses, SERVFAIL, REFUSED, and
forwarding failures SHALL surface as distinct typed outcomes. The
connector SHALL NOT fabricate answers and SHALL NOT retry indefinitely.

#### Scenario: NXDOMAIN is typed
- **WHEN** the upstream answers NXDOMAIN
- **THEN** the guest SHALL receive a `DnsResponse` with the NXDOMAIN
  outcome, distinct from a timeout

#### Scenario: Upstream error codes are typed
- **WHEN** the upstream answers SERVFAIL or REFUSED
- **THEN** the guest SHALL receive the corresponding typed outcome,
  distinct from `Ok`

#### Scenario: Forwarding failure is typed
- **WHEN** the connector cannot encode or send the query, or the reply is
  undecodable
- **THEN** the guest SHALL receive the `Upstream` outcome, distinct from
  a timeout

### Requirement: Well-Known Discovery Registration
The connector's well-known URI (`sel://sys/dns/resolve`) SHALL be
registered with discovery so guests can attach at runtime; the runtime
performs the registration at provision time on the connector's behalf
(host listener queue injected as the leading entrypoint argument), and
resolution authority SHALL be expressible as a channel grant on that URI.
Registration SHALL be revoked when the connector terminates.

#### Scenario: Tenant-scoped resolution policy
- **WHEN** a tenant's grant includes the connector's well-known channel
- **THEN** that tenant's guests MAY resolve names, and tenants without
  the grant SHALL NOT