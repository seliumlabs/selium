## ADDED Requirements

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
