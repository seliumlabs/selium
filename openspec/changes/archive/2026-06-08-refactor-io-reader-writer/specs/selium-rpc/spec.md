## MODIFIED Requirements

### Requirement: Typed RPC Client
`selium-guest::io::rpc` SHALL provide an `RpcClient<Req, Rep>` that sends typed requests and receives typed replies over shared-memory ring buffers. The client SHALL use `FramedWrite` for the request ring and `FramedRead` for the reply ring, rather than working with `RingBuf` directly.

#### Scenario: Client sends request and receives reply
- **WHEN** a client calls `RpcClient::request(req)` with a valid request payload
- **THEN** the payload SHALL be rkyv-encoded and written as a frame to the request ring buffer via `FramedWrite::write_frame`, and the client SHALL block on the reply ring buffer's generation counter until a reply frame is ready via `FramedRead::read_frame`

#### Scenario: Client handles disconnected server
- **WHEN** a client attempts to send a request after the server has disconnected
- **THEN** the client SHALL detect the writer count on the reply ring has reached zero and return an `RpcError::ConnectionClosed`

### Requirement: Typed RPC Connection
`selium-guest::io::rpc` SHALL provide an `RpcConnection<Req, Rep>` that receives typed requests and allows sending typed replies. The connection SHALL use `FramedRead` for the request ring and `FramedWrite` for the reply ring.

#### Scenario: Server receives request and sends reply
- **WHEN** a server calls `RpcConnection::recv()` and a request frame is available on the request ring
- **THEN** the server SHALL decode the request via `FramedRead::read_frame` and return an `RpcRequest` that can call `.reply(response)` to write the reply via `FramedWrite::write_frame`

#### Scenario: Server detects client disconnect
- **WHEN** a server calls `RpcConnection::recv()` and the writer count on the request ring has reached zero
- **THEN** the server SHALL return `RpcError::ConnectionClosed`

### Requirement: RPC Connection Handshake via HostQueue
`selium-guest::io::rpc` SHALL establish RPC connections by having the client allocate a shared region, send its `shared_id` through a `HostQueue`, and both sides attach to that region.

#### Scenario: Client connects to server
- **WHEN** a client calls `RpcClient::connect(sender, req_capacity, rep_capacity)`
- **THEN** the client SHALL allocate a shared region, enqueue its `shared_id` via `ResourceSender::send`, attach to the region, and return an `RpcClient` ready for requests

#### Scenario: Server accepts client connection
- **WHEN** a server receives an `IncomingConnection` via `ResourceListener::recv()`
- **THEN** `RpcAccept::accept` SHALL attach to the region identified by `IncomingConnection::shared_id` and return an `RpcConnection` ready to receive requests

### Requirement: RPC Session Region Layout
RPC session shared regions SHALL contain two ring buffers: a request ring (client writes, server reads) and a reply ring (server writes, client reads), laid out using the standard multi-memory region header.

#### Scenario: Client and server agree on ring layout
- **WHEN** both client and server attach to the same RPC session region
- **THEN** both SHALL discover the request ring at entry index 0 and the reply ring at entry index 1 via the multi-memory region header

### Requirement: RPC Request Correlation
The `RpcRequest` type SHALL support replying exactly once, using the frame `tag` field for request/reply correlation.

#### Scenario: Server replies to a specific request
- **WHEN** a server calls `request.reply(response)` on an `RpcRequest`
- **THEN** the reply frame SHALL carry the same `tag` value as the request frame, allowing the client to correlate the response

## REMOVED Requirements

None — all existing RPC requirements are retained, with the implementation location changed from `crates/patterns/rpc/` to `crates/core/guest/src/io/rpc.rs`.
