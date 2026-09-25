## Purpose

Define the typed RPC client and connection types in `selium-wire`, generic over `MessageTransport`, enabling request/reply communication between peers over shared-memory rings, QUIC streams, or any other transport.

## Requirements

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

### Requirement: Server-Streaming RPC
`selium-wire` SHALL provide a server-streaming RPC pattern generic over
`MessageTransport`: one request frame yields an ordered stream of reply
frames sharing the request's correlation tag, terminated by a stream-end
flag or an error frame carrying `FLAG_STREAM_ERROR` with the server's
error message as its payload.

#### Scenario: Ordered reply stream
- **WHEN** a client issues a server-streaming call and the server
  produces three items then ends the stream
- **THEN** the client SHALL observe the three items in order followed by
  end-of-stream, all frames carrying the request's tag

#### Scenario: Mid-stream error
- **WHEN** the server fails after emitting two items and terminates the
  stream with an error frame
- **THEN** the client's stream SHALL yield the two items and then an
  error preserving the server's message

### Requirement: Bidi-Streaming RPC
`selium-wire` SHALL provide a bidi-streaming RPC pattern over one
correlation tag, with independent send and receive halves; each
direction closes independently via its own stream-end flag.

#### Scenario: Half-close
- **WHEN** the client ends its send direction
- **THEN** the server SHALL observe end-of-stream on its receive half
  and SHALL still be able to send remaining replies before ending its
  own direction

### Requirement: Stream Cancellation
The client SHALL be able to cancel an in-flight stream. On cancel, the
server SHALL stop producing items and release per-stream resources.
Dropping a client stream handle SHALL send a cancel frame. Symmetrically,
a server that abandons an unfinished stream SHALL send a cancel frame on
its reply direction so the client observes termination deterministically
rather than via peer-close detection.

#### Scenario: Cancel stops production
- **WHEN** a client cancels after receiving one item of a long stream
- **THEN** the server SHALL emit no further items for that tag

#### Scenario: Server abandonment notifies client
- **WHEN** the server drops an unfinished stream request handle while
  keeping the connection open
- **THEN** the client's stream SHALL terminate without relying on peer
  close or timeout

### Requirement: Streaming Backpressure Honesty
Stream writers SHALL park on a full ring using the generation-wait
mechanism. The pattern layer SHALL NOT introduce unbounded buffering,
and streams SHALL NOT report `Overwritten` losses.

#### Scenario: Slow consumer
- **WHEN** a consumer reads slower than the producer emits and the ring
  fills
- **THEN** the producer SHALL park until capacity frees, and no item
  SHALL be lost or silently dropped
