# Tasks: HTTP Connector

## 1. Protocol types

- [x] 1.1 Create `selium-proto-http` crate: `HttpRequest`, `HttpResponse`, `HttpBodyChunk`, trailer types via `selium-encoding` schema macros
- [x] 1.2 Schema round-trip tests incl. chunked body sequences

## 2. Connector guest

- [x] 2.1 Create `selium-connector-http` guest crate (plain guest on `selium-guest::net`)
- [x] 2.2 TCP listener + accept loop spawning per-connection channel pairs
- [x] 2.3 TLS termination with rustls; certificate/key loaded via storage grant; loud failure on missing/invalid cert material
- [x] 2.4 HTTP/1.1 codec: request parse → `HttpRequest`; typed `HttpResponse` → wire bytes; keep-alive ordering
- [x] 2.5 Discovery route resolution (Host + path → channel) with cache and invalidation
- [x] 2.6 In-flight correlation map (connection ordering ↔ frame tags)
- [x] 2.7 Backpressure: ring-full stops socket reads; resume on generation advance
- [x] 2.8 Streaming bodies mapped to server-streaming RPC (dependency: `streaming-rpc-patterns`)

## 3. App-guest API

- [x] 3.1 Typed serve API: register served URI subtree, receive `RpcConnection<HttpRequest, HttpResponse, _>` (+ server-stream variant)
- [x] 3.2 Document the zero-`Network`-grant capability model and `ExplicitResource` channel hygiene

## 4. Verification

- [x] 4.1 Golden path: HTTP request bytes → connector pipeline → typed forwarding seam → typed response on the wire (`cargo test -p selium-connector-http`); substrate-level channel handoff covered by `cargo test -p selium-runtime --test http_connector`. External `curl`-against-a-live-listener coverage requires the wasm TLS story (see follow-ups) and is deferred with it.
- [x] 4.2 CI: pipelined keep-alive requests carry distinct correlation tags and responses are emitted on the wire in request order even when completion order differs (`selium-connector-http` pipeline tests); host-queue FIFO delivery substrate in `selium-runtime` tests
- [x] 4.3 CI: full pipeline window stops socket reads, resumes on completion, loses no request bytes (`selium-connector-http` pipeline tests); full-ring parks writers until the reader drains (`selium-runtime` tests, substrate)
- [x] 4.4 CI: app guest with no Network grants serves successfully; attach to a connection region by an ungranted third party (broad class grant, no ownership/ExplicitResource) is denied (`selium-runtime` tests)
- [x] 4.5 CI: chunked request bodies decode into typed requests; streamed responses reach the wire as chunked transfer encoding with trailers; oversized requests get typed 413; concurrent connections are independent (`selium-connector-http` pipeline tests)
