# Design: Streaming RPC Pattern Variants

## Context

`selium-wire` has unary RPC over `MessageTransport` with frame-tag
correlation (`transport-abstraction`). Connectors (`http-connector`)
need streaming shapes; the pattern catalog is one variant short.

## Goals / Non-Goals

**Goals:**

- Server-streaming and bidi-streaming RPC over any `MessageTransport`
- Cancellation and half-close as first-class frame semantics
- Backpressure honesty (Park means Park; no unbounded buffers)

**Non-Goals:**

- Flow-control windows/credit-based streaming (ring capacity + Park is
  the v1 flow control)
- Multiplexing multiple streams over one transport beyond what tags
  already provide
- Changes to unary RPC semantics

## Decisions

### Streams ride existing frame headers

Tag = correlation (all frames of a stream share the request's tag, per
`transport-abstraction`); three new flag bits on `FrameHeader.flags`:
stream item, stream end, stream cancel. No new transport nouns
(non-negotiable 6). Alternative considered: a stream-id field — rejected
as redundant with the tag.

### Half-close mirrors TCP discipline

Each direction ends via its own end flag; the peer may keep sending.
This matches the `TcpStream` EOF/half-close model in `guest-networking`
and maps directly onto WebSocket close and HTTP body boundaries.

### Cancellation is a frame, not a drop

Dropping a stream handle sends a cancel frame on both sides — client
handles cancel the server's production; server request handles notify the
client of abandonment. Resources are released deterministically rather
than via peer-close detection (timeouts). Alternative considered: rely on
`writer_count`/peer-closed — rejected because a cancelled stream is not a
dead peer.

### Writers park via an async write path

Stream senders use `FramedWrite::write_frame_with_flags_async`, which
propagates transport `Pending` to the awaiting task instead of collapsing
it into `BufferFull` (the sync path keeps `BufferFull` semantics for
unary RPC and pub/sub). This requires two transport-side properties:
Park channels must expose a blocking reader (slot-protected, so writer
backpressure engages at all), and reader consumption must wake parked
writers (`wake_generation_waiters` on advance), since only writes bump
the generation counter.

## Risks / Trade-offs

- Flag-bit discipline drift across transports → conformance tests run
  the same stream suite over `ShmTransport` (and `QuicTransport` when
  reinstated).
- Cancel-after-end races → spec'd as harmless: cancel for a completed
  tag is ignored.
