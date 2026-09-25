## Purpose

Define the HTTP connector: a system guest that terminates external
TCP/TLS/HTTP-1.1 at the edge and forwards typed, schema-encoded HTTP
messages over shared-memory channels, so application guests serve web
traffic with no network capabilities of their own.

## ADDED Requirements

### Requirement: Edge Termination of HTTP/1.1 over TLS
The connector SHALL terminate TCP, TLS, and HTTP/1.1 at the edge. The
external wire encoding SHALL be real HTTP/1.1 over TLS; browsers and
`curl` SHALL be able to talk to a connector-served guest without any
Selium client software.

#### Scenario: Browser-grade request
- **WHEN** an external client issues an HTTPS/1.1 request to the
  connector's listener
- **THEN** the connector SHALL complete the TLS handshake, parse the
  request, and forward a typed `HttpRequest` to the serving guest

#### Scenario: Missing certificate material
- **WHEN** the connector starts without loadable certificate material
- **THEN** it SHALL fail loudly at startup and SHALL NOT serve plaintext
  HTTP on the TLS listener

### Requirement: Discovery-Based Route Resolution
The connector SHALL resolve the serving channel for a request via
discovery, matching Host and path against registered URI subtrees. The
connector SHALL NOT hold a static routing table.

#### Scenario: Request routed to registered guest
- **WHEN** a request arrives for a Host/path under a registered subtree
- **THEN** the connector SHALL forward the typed request on the resolved
  channel

#### Scenario: No route registered
- **WHEN** no registration matches the request's Host/path
- **THEN** the connector SHALL respond with a typed 404-equivalent
  response without contacting any app guest

### Requirement: Typed Forwarding with Tag Correlation
Forwarded requests SHALL be schema-encoded `HttpRequest` messages;
responses SHALL be schema-encoded `HttpResponse` messages. Correlation
between protocol-level request ordering and frame tags SHALL be
preserved so concurrent keep-alive requests on one connection receive
correctly ordered responses.

#### Scenario: Concurrent keep-alive requests
- **WHEN** two requests arrive pipelined on one connection
- **THEN** each typed request SHALL carry a distinct tag, and responses
  SHALL be emitted on the wire in request order with the matching bodies

### Requirement: Zero-Network-Grant App Guests
App guests served by the connector SHALL require no `Network`
capability grants — only channel attach grants scoped to their
connection regions (recommended: `ExplicitResource` per connection).
Broad shared-memory `UriPrefix` grants SHALL be documented as an
anti-pattern for connector-served channels.

#### Scenario: App guest serves with no Network grant
- **WHEN** an app guest holding only channel attach grants is registered
  for a URI subtree
- **THEN** it SHALL receive and answer typed HTTP requests successfully

#### Scenario: Ungranted third party cannot intercept
- **WHEN** a guest without a grant for a connection region attempts
  `attach_region` on it
- **THEN** the runtime SHALL deny the attach

### Requirement: Edge Backpressure Honesty
The connector SHALL translate channel backpressure into socket flow
control: when a serving channel's ring is full, the connector SHALL stop
reading from the client socket until capacity frees. The connector SHALL
NOT buffer unboundedly.

#### Scenario: Slow app guest
- **WHEN** an app guest consumes responses slower than the client sends
  and the ring fills
- **THEN** the connector SHALL pause socket reads and resume on
  generation advance, with no request bytes lost

### Requirement: Streaming Bodies via Server-Streaming RPC
Streamed HTTP response bodies SHALL be mapped to server-streaming RPC
(`streaming-rpc-patterns`): routes registered with the streaming
interface deliver a typed head followed by body chunks and optional
trailers, which the connector SHALL write to the wire incrementally with
chunked transfer encoding. The connector SHALL NOT buffer an entire
streamed body at the edge. Requests with chunked bodies SHALL be decoded
at the edge into the typed request's inline body.

#### Scenario: Chunked response streamed to the wire
- **WHEN** a serving guest produces a streamed response (head, chunks,
  trailers) over a server-streaming session
- **THEN** the connector SHALL write chunked transfer encoding
  incrementally, with chunks and trailers in produced order and the
  stream terminated by the zero-length chunk

#### Scenario: Oversized request rejected at the edge
- **WHEN** a request exceeds the edge's inline size limit
- **THEN** the connector SHALL respond with a typed 413-equivalent error
  and SHALL NOT forward the request

### Requirement: Raw-Path Coexistence
The connector SHALL be one framing of the shared substrate, not the only
one: guests using raw `TcpStream`/`TcpListener` directly (BYO framework)
SHALL be unaffected by the connector's existence, and both models MAY
run on different listeners simultaneously.

#### Scenario: BYO framework alongside connector
- **WHEN** one guest serves via the connector and another terminates its
  own TLS+HTTP on a raw listener
- **THEN** both SHALL operate independently with their own grants
