## Purpose

Define the DNS connector: an egress-only system guest that performs real
DNS resolution on behalf of other guests, exposing it as a typed,
capability-gated RPC so that no ambient name resolution exists in host
or runtime paths.

## ADDED Requirements

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
Upstream timeout, NXDOMAIN, and truncated responses SHALL surface as
distinct typed outcomes. The connector SHALL NOT fabricate answers and
SHALL NOT retry indefinitely.

#### Scenario: NXDOMAIN is typed
- **WHEN** the upstream answers NXDOMAIN
- **THEN** the guest SHALL receive a `DnsResponse` with the NXDOMAIN
  outcome, distinct from a timeout

### Requirement: Well-Known Discovery Registration
The connector SHALL register a well-known URI with discovery so guests
can attach at runtime; resolution authority SHALL be expressible as a
channel grant on that URI.

#### Scenario: Tenant-scoped resolution policy
- **WHEN** a tenant's grant includes the connector's well-known channel
- **THEN** that tenant's guests MAY resolve names, and tenants without
  the grant SHALL NOT
