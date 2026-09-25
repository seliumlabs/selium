# selium-client Specification

## Purpose
TBD ... Update Purpose after archive

## Requirements

### Requirement: QUIC Connection
The crate SHALL provide a `connect` operation that establishes a single QUIC (TLS 1.3) connection to a Selium server address, trusting a caller-provided server certificate, and returns a `Client` handle owning the connection.

#### Scenario: Client connects
- **WHEN** a user calls `client::connect(addr, tls_options)` with a server certificate to trust
- **THEN** the crate SHALL complete the QUIC handshake and return a `Client` for opening channels

### Requirement: Mutual TLS Client Identity
The crate SHALL present a client certificate chain and key, supplied by the caller, when establishing the connection, so the connector can authenticate the client's identity. Provisioning of that identity material SHALL remain out of scope.

#### Scenario: Client presents identity
- **WHEN** identity material (certificate chain + key) is provided in the connect options
- **THEN** the connection SHALL complete mutual TLS so the connector derives the client's tenant and fingerprint

### Requirement: Channel Open Per Stream
Each channel handle SHALL open a fresh bidirectional QUIC stream and send a typed bridge handshake naming the channel's `sel://` URI. The handshake SHALL be deterministic: the channel open SHALL await the bridge's typed control reply — acceptance on success, termination on refusal — and only return a handle once the bridge confirms the channel is attached. Distinct channel handles SHALL map to distinct streams on one connection. The reply SHALL be read through the same framed reader the handle keeps, so relayed data frames that arrive alongside or after the reply are preserved.

#### Scenario: Open a channel
- **WHEN** a user opens a channel for `sel://acme/lobby`
- **THEN** a new stream SHALL open, carry the handshake naming that URI, await the bridge's acceptance reply, and return a typed handle

### Requirement: Typed Messaging Handles
The crate SHALL expose typed messaging handles with transport generics hidden: a subscriber as a `futures::Stream` of `T`, a publisher as a `futures::Sink<T>`, and request/response plus streaming RPC handles. The user SHALL supply the message type (`T: FlatMsg`) by importing the appropriate flatbuffers bindings.

#### Scenario: Subscribe to a channel
- **WHEN** a user calls `client.subscriber::<Biscuit>("sel://acme/lobby").await`
- **THEN** the returned handle SHALL yield `Biscuit` values as a stream

#### Scenario: Publish to a channel
- **WHEN** a user publishes a value through a publisher handle
- **THEN** the value SHALL be encoded and sent as a framed message on the channel's stream

### Requirement: Typed Termination Errors
The crate SHALL map bridge termination frames (`PipeControl::Terminate`) to typed channel errors, surfacing handshake and attach failures as typed errors at channel open (the bridge replies deterministically, so refusal is observable without waiting for data).

#### Scenario: Channel open fails
- **WHEN** the bridge cannot resolve or attach the requested channel
- **THEN** the channel open SHALL return an error carrying the termination code (bad handshake vs attach failure), for every handle type

### Requirement: Unified Error Surface
Connection, TLS, transport, framing, RPC, and termination failures SHALL surface through one crate error type. Underlying `selium-wire` errors SHALL pass through a dedicated wire variant; failures the client can classify (termination codes, remote stream errors, serialization) SHALL map to dedicated variants.

#### Scenario: Connection failure surfaces typed error
- **WHEN** a connect or channel open fails for any reason
- **THEN** the crate SHALL return an error from its single error type

### Requirement: Encoding Re-exports
The crate SHALL re-export the `FlatMsg` trait and the encoding crate so users can import message bindings without wiring additional crates.

#### Scenario: Single import surface
- **WHEN** a user writes a `publisher::<T>`/`subscriber::<T>` handle
- **THEN** the required `FlatMsg` bound SHALL be importable from `selium-client`
