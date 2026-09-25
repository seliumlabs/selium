# Tasks: Streaming RPC Pattern Variants

## 1. Frame discipline

- [x] 1.1 Define stream flag bits (item / end / cancel / error) on the existing `FrameHeader` flags field, documented in `framed-io` terms
- [x] 1.2 Confirm tag-correlation invariants: all frames of one stream share the request's tag

## 2. Server-streaming

- [x] 2.1 `RpcServerStreamClient<Req, Item, M>`: `call(req) -> impl Stream<Item = Result<Item>>`
- [x] 2.2 Server side: connection handler produces items until end flag; error terminates with an error frame (`FLAG_STREAM_ERROR` + message payload)
- [x] 2.3 Cancellation: client cancel frame → server stops producing; client dropping the stream sends cancel; server dropping an unfinished stream notifies the client with cancel

## 3. Bidi-streaming

- [x] 3.1 `RpcBidiStream<Req, Item, Resp, M>`: independent send/receive halves over one tag
- [x] 3.2 Half-close semantics per direction (end flag = that direction closed; peer may continue)

## 4. Backpressure + errors

- [x] 4.1 Writers park on full ring via async write path + generation wait (sync path keeps `BufferFull`; Park channels use blocking readers; reader advance wakes parked writers)
- [x] 4.2 Transport errors map into the stream's error item per the `MessageTransport` error abstraction
- [x] 4.3 Connection-level recv drains stale lifecycle frames so cancels/ends never surface as bogus requests

## 5. Verification

- [x] 5.1 Tests over `ShmTransport`: ordered delivery, end-of-stream, cancel, error mid-stream
- [x] 5.2 Backpressure test: slow consumer + small ring → producer parks via async send, no item loss, no spin
- [x] 5.3 Overwrite semantics: streams never report `Overwritten` (bounded/park), asserted in tests under ring overflow
- [x] 5.4 Server-drop notification test: abandoned stream terminates the client deterministically; stale cancel frames are skipped for subsequent requests
