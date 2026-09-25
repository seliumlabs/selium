## Purpose

Define the runtime-mediated path from an edge connector's route
resolution to a live typed RPC session with the serving application
guest: host-queue attach on resolved routes, per-connection region
lifecycle, receiver rights derived from queue handoff, and failure
semantics for stale routes. Protocol-agnostic — HTTP, DNS, and future
connectors consume this flow unchanged.

## Requirements

### Requirement: Route Resolution to Typed Session

A connector SHALL establish a typed RPC session with a serving guest by
resolving the target URI subtree via discovery, attaching to the
resolved host queue as a sender, allocating a request/reply shared-memory
region pair, and delivering the region id to the serving guest through
that queue. The serving guest SHALL receive the session as a typed RPC
connection carrying schema-encoded messages. The composed flow SHALL
introduce no transport mechanism beyond those defined by
`discovery-registration`, `secure-rpc`, and `capability-enforcement`.

#### Scenario: Forwarded request reaches registered guest

- **WHEN** a connector resolves a request's URI subtree to a registered
  host queue and completes the rendezvous
- **THEN** the serving guest receives a typed RPC session over the newly
  allocated region pair, and messages sent by the connector arrive at
  the serving guest with correlation tags preserved

#### Scenario: Session establishment contacts only the resolved guest

- **WHEN** a connector establishes a session for a resolved route
- **THEN** no guest other than the resolved serving guest and the
  connector hold rights on the session regions

### Requirement: Per-Connection Region Lifecycle

The connector SHALL allocate one dedicated request/reply region pair per
forwarded external connection. Regions SHALL NOT be shared across
external connections. The connector SHALL reclaim a connection's regions
when the external peer disconnects or the serving guest closes the
session, whichever first.

#### Scenario: Distinct regions per external connection

- **WHEN** a connector forwards two concurrent external connections to
  the same serving guest
- **THEN** each connection operates on its own region pair with distinct
  region ids

#### Scenario: Reclamation on disconnect

- **WHEN** an external peer disconnects after session establishment
- **THEN** the connector frees the connection's region pair

#### Scenario: Reclamation on serving-guest close

- **WHEN** the serving guest closes the RPC session while the external
  peer remains connected
- **THEN** the connector observes session closure, stops forwarding for
  that connection, and frees the region pair

### Requirement: Queue-Derived Receiver Rights

A serving guest SHALL NOT be required to hold pre-existing grants for
session regions. When a guest receives a region id through a host queue
it is legitimately attached to, ownership SHALL be shared with the
receiver at receive time per `capability-enforcement`, and the
subsequent region attach SHALL succeed. App guests served through this
flow SHALL require no `Network` capability grants.

#### Scenario: Zero-grant serving guest attaches session region

- **WHEN** an app guest holding only channel-attach grants receives a
  region id via its registered listener queue and calls attach
- **THEN** the attach succeeds and the guest can serve requests on the
  session

#### Scenario: Third party cannot claim handoff rights

- **WHEN** a guest not attached to the listener queue learns a session
  region id by any other means and attempts attach
- **THEN** the attach is denied per `capability-enforcement`

### Requirement: Stale Route Failure Semantics

When a connector's resolved route refers to a queue that no longer
exists or that the connector cannot legitimately attach to, the forward
attempt SHALL fail loudly at queue attach. Requests SHALL NOT be
misrouted to an unintended guest, silently dropped beyond the failed
request, or delivered to a replacement queue chosen implicitly.

#### Scenario: Revoked route fails loudly

- **WHEN** a serving guest revokes its URI subtree and a connector
  attempts to forward using a previously cached resolution
- **THEN** the queue attach fails, the connector reports the failure to
  the affected request only, and no unrelated guest receives traffic

#### Scenario: Failed forward does not poison the connector

- **WHEN** a forward attempt fails due to a stale route
- **THEN** the connector continues accepting and routing subsequent
  requests, re-resolving routes rather than reusing the failed cache
  entry indefinitely
