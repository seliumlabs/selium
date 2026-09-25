## MODIFIED Requirements

### Requirement: Typed RPC Client
`selium-wire` SHALL provide an `RpcClient<Req, Rep, M>` generic over `M: MessageTransport` that sends typed requests and receives typed replies. The client SHALL use `FramedWrite<M>` for the request channel and `FramedRead<M>` for the reply channel. Connection establishment SHALL be parameterized by a `Rendezvous` trait rather than hard-wired to `ResourceSender`.

#### Scenario: Client sends request and receives reply
- **WHEN** a client calls `RpcClient::request(req)` with a valid request payload
- **THEN** the payload SHALL be encoded via `FlatMsg::encode` and written as a frame to the request transport via `FramedWrite::write_frame`, and the client SHALL poll the reply transport's generation counter until a reply frame with matching correlation tag is available via `FramedRead::read_frame`

#### Scenario: Client handles disconnected server
- **WHEN** a client calls `poll_peer_closed` on the reply transport and it returns `true`
- **THEN** the client SHALL return `RpcError::ConnectionClosed`

### Requirement: Typed RPC Connection
`selium-wire` SHALL provide an `RpcConnection<Req, Rep, M>` generic over `M: MessageTransport` that receives typed requests and allows sending typed replies. The connection SHALL use `FramedRead<M>` for the request channel and `FramedWrite<M>` for the reply channel.

#### Scenario: Server receives request and sends reply
- **WHEN** a server calls `RpcConnection::recv()` and a request frame is available on the request transport
- **THEN** the server SHALL decode the request via `FramedRead::read_frame` and return an `RpcRequest` that can call `.reply(response)` to write the reply via `FramedWrite::write_frame`

#### Scenario: Server detects client disconnect
- **WHEN** a server calls `RpcConnection::recv()` and the request transport signals peer closed
- **THEN** the server SHALL return `RpcError::ConnectionClosed`

### Requirement: RPC Connection Handshake via Rendezvous
`selium-wire` SHALL establish RPC connections by having the client allocate request and reply channels (via a `Rendezvous` trait), then both sides attach to the channels. The `Rendezvous` trait SHALL abstract the mechanism for passing connection identifiers between client and server.

#### Scenario: Client connects to server via shm rendezvous
- **WHEN** a client calls `RpcClient::connect(rendezvous, req_capacity, rep_capacity)` with a host-queue-backed `Rendezvous`
- **THEN** the client SHALL allocate shared-memory regions, send their identifiers through the rendezvous, and return an `RpcClient<_, _, ShmTransport>` ready for requests

#### Scenario: Server accepts client connection via shm rendezvous
- **WHEN** a server receives a connection via `ResourceListener::accept::<RpcAccept<Req, Rep>>()`
- **THEN** `RpcAccept::accept` SHALL attach to the shared regions and return an `RpcConnection<_, _, ShmTransport>` ready to receive requests

### Requirement: RPC Session Layout
RPC sessions SHALL use two independent `MessageTransport` channels: a request channel (client writes, server reads) and a reply channel (server writes, client reads). When using `ShmTransport`, these SHALL be arranged as a multi-memory region with two ring buffers. When using `QuicTransport`, these SHALL be two QUIC streams (or one bidirectional stream with stream-level correlation).

#### Scenario: Client and server agree on channel layout for shm
- **WHEN** both client and server attach via a shared region rendezvous
- **THEN** both SHALL discover the request ring at entry index 0 and the reply ring at entry index 1 via the multi-memory region header

### Requirement: RPC Request Correlation
The `RpcRequest` type SHALL support replying exactly once, using the frame `tag` field for request/reply correlation. Correlation SHALL be transport-independent.

#### Scenario: Server replies to a specific request
- **WHEN** a server calls `request.reply(response)` on an `RpcRequest`
- **THEN** the reply frame SHALL carry the same `tag` value as the request frame, allowing the client to correlate the response
