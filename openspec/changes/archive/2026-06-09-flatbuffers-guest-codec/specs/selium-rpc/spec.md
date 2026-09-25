## MODIFIED Requirements

### Requirement: Typed RPC Client
`selium-guest::io::rpc` SHALL provide an `RpcClient<Req, Rep>` that sends typed requests and receives typed replies over shared-memory ring buffers. The client SHALL use `FramedWrite` for the request ring and `FramedRead` for the reply ring, rather than working with `RingBuf` directly.

Both `Req` and `Rep` SHALL implement `FlatMsg` for serialization. The framing layer remains codec-agnostic.

#### Scenario: Client sends request and receives reply
- **WHEN** a client calls `RpcClient::request(req)` with a valid request payload
- **THEN** the payload SHALL be Flatbuffers-encoded via `FlatMsg::encode` and written as a frame to the request ring buffer via `FramedWrite::write_frame`, and the client SHALL block on the reply ring buffer's generation counter until a reply frame is ready via `FramedRead::read_frame`

#### Scenario: Client handles disconnected server
- **WHEN** a client attempts to send a request after the server has disconnected
- **THEN** the client SHALL detect the writer count on the reply ring has reached zero and return an `RpcError::ConnectionClosed`

### Requirement: Typed RPC Connection
`selium-guest::io::rpc` SHALL provide an `RpcConnection<Req, Rep>` that receives typed requests and allows sending typed replies. The connection SHALL use `FramedRead` for the request ring and `FramedWrite` for the reply ring.

Both `Req` and `Rep` SHALL implement `FlatMsg` for serialization.

#### Scenario: Server receives request and sends reply
- **WHEN** a server calls `RpcConnection::recv()` and a request frame is available on the request ring
- **THEN** the server SHALL decode the request via `FramedRead::read_frame` and return an `RpcRequest` that can call `.reply(response)` to write the reply via `FramedWrite::write_frame`

#### Scenario: Server detects client disconnect
- **WHEN** a server calls `RpcConnection::recv()` and the writer count on the request ring has reached zero
- **THEN** the server SHALL return `RpcError::ConnectionClosed`

### Requirement: RPC Request Correlation
The `RpcRequest` type SHALL support replying exactly once, using the frame `tag` field for request/reply correlation. The `RpcRequest::payload()` and `RpcRequest::into_payload()` methods SHALL decode the request body via `FlatMsg::decode::<Req>`, and `RpcRequest::reply()` SHALL encode the response via `FlatMsg::encode`.

#### Scenario: Server replies to a specific request
- **WHEN** a server calls `request.reply(response)` on an `RpcRequest`
- **THEN** the reply frame SHALL carry the same `tag` value as the request frame, allowing the client to correlate the response

#### Scenario: Server decodes request payload via FlatMsg
- **WHEN** a server calls `request.payload()` on an `RpcRequest`
- **THEN** the raw frame payload bytes SHALL be decoded via `FlatMsg::decode::<Req>`
