# Proposal: Streaming RPC Pattern Variants

## Why

`selium-wire` today offers unary RPC (`RpcClient`/`RpcConnection`),
pub/sub, and tables. The connector model (see `http-connector`) maps
wire protocols onto messaging patterns, and the mapping table has holes
that only streaming variants fill: HTTP chunked/SSE responses are
server-streaming, WebSocket is a bidi message stream, and large request
bodies are client-streaming. Without these variants, connectors would
either buffer entire bodies at the edge (dishonest backpressure) or
misuse unary RPC.

## What Changes

- **Server-streaming RPC**: one request frame → ordered stream of reply
  frames, terminated by a stream-end flag or an error frame.
- **Bidi-streaming RPC**: both directions stream ordered frames over one
  correlation tag; either side may end its direction independently
  (mirrors TCP half-close discipline).
- **Cancellation**: the client may send a stream-cancel frame; servers
  SHALL honour it promptly (stop producing, release resources).
- **Backpressure honesty**: stream writers park on a full ring via the
  generation-wait mechanism (Park means Park, per `channel-wake-wait`);
  no unbounded buffering at the pattern layer.
- Frame-format reuse: streams ride the existing `FrameHeader`
  (tag = correlation, per `transport-abstraction`); new flag bits
  denote stream item / stream end / stream cancel. No new transport
  nouns — non-negotiable 6 holds.

## Capabilities

### New Capabilities

(None.)

### Modified Capabilities

- `selium-rpc`: server-streaming and bidi-streaming RPC variants,
  cancellation, streaming backpressure honesty

Pattern-layer change; transports and their grants are untouched.

## Impact

- `selium-wire`: new `RpcServerStream`/`RpcBidiStream` client and
  connection types, generic over `MessageTransport`, same error
  abstraction rules as unary RPC.
- Specs: MODIFIED `selium-rpc`.
- Consumers: `http-connector` (SSE/chunked/upgrade), future WebSocket
  connector.
