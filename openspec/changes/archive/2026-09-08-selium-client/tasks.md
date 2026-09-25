## 1. selium-wire waker-honest read path

- [x] 1.1 Add a waker-honest async read path (`poll_frame`/`read_frame_async`) to `FramedRead`, selected per transport (generation-tracked reads unchanged)
- [x] 1.2 Route `Subscriber` reads over the waker-honest path for transports without a generation counter, and verify a native test shows a `Subscriber` awaits parking until a frame arrives (no busy loop)
- [x] 1.3 Route the generation-wait/yield fallbacks in `pubsub`/`rpc`/`stream` to life only for generation-tracked transports, and verify the existing `selium-wire` tests pass
- [x] 1.4 Provide an async `LiveTable` sync path for non-ring transports, and verify a native live-table test reads remote writes without spinning

## 2. Shared handshake contract

- [x] 2.1 Move `PipeControl` (handshake/termination) from `selium-bridge-channel` into `selium-wire` and verify `cargo test -p selium-bridge-channel` passes importing it back
- [x] 2.2 Re-export the termination codes (bad handshake, attach failed) with the shared type, and verify the bridge denial test still observes the same frame
- [x] 2.3 Add the deterministic success reply (`PipeControl::Accepted`): the bridge-channel sends it after resolve/attach, immediately before the splice, and verify a native test observes the accepted frame (and that the fabric-close teardown test accounts for it)
- [x] 2.4 Verify the handshake remains refusal-only-compatible where it must: `Terminate` (bad handshake, attach failed) still precedes stream teardown, verified by the bridge denial tests

## 3. Client crate scaffold

- [x] 3.1 Create `crates/client` with a `selium-client` package, add it as a workspace member, and verify `cargo check -p selium-client` succeeds
- [x] 3.2 Wire the `tokio`-featured `selium-wire`, quinn, rustls/rustls-pemfile, tokio, and `selium-encoding` dependencies, and verify the dependency graph resolves

## 4. QUIC + TLS plumbing

- [x] 4.1 Implement `MessageTransport` adapters over `quinn::RecvStream`/`SendStream` and verify a duplex test round-trips frames through them
- [x] 4.2 Implement a TLS config builder (server root certs + optional client identity chain/key) and verify a handshake test against the connector's native seam completes
- [x] 4.3 Implement `Client::connect` returning a `Client` owning the connection, and verify a connect test succeeds against the handshake seam with a trusted root

## 5. Channel handles

- [x] 5.1 Implement channel open (fresh bidi stream + `PipeControl::Handshake { uri }` + await the deterministic `Accepted`/`Terminate` reply), and verify a test writes the handshake, observes the accepted reply, then a data frame with a correlation tag
- [x] 5.2 Implement `subscriber::<T>(uri)` as `futures::Stream` and verify a test reads decoded values end-to-end (including a data frame that arrives alongside the accepted reply — read-ahead preserved)
- [x] 5.3 Implement `publisher::<T>(uri)` as `futures::Sink<T>` and verify a test sends an encoded message
- [x] 5.4 Implement `rpc::<Req, Rep>(uri)` plus server-streaming and bidi-streaming handles, and verify request/reply plus a server-streaming round-trip and a bidi-streaming round-trip (typed fabric-side connections drive the bridge halves)
- [x] 5.5 Implement `live_table::<K, V>(uri)` over the async sync path, and verify set/get/delete round-trip on a native transport

## 6. Errors and re-exports

- [x] 6.1 Implement the unified crate error: termination codes map to typed variants at channel open for every handle type (subscriber, publisher, RPC, streaming, live-table), and verify denied-attach and bad-handshake tests return the typed errors at open
- [x] 6.2 Implement error pass-through/enrichment: `From<selium_wire::Error>` (wire pass-through, serialization enriched) and `From<RpcError>` (remote/serialization/closed semantics), surfaced by every handle newtype
- [x] 6.3 Re-export `FlatMsg` and the encoding crate, and verify a user type compiles from the `selium-client` import alone

## 7. Integration

- [x] 7.1 Add a native integration test driving `connect` + a channel round-trip against the connector handshake seam, and verify it passes
- [x] 7.2 Once `addressing` lands, replace `quic_spine`'s hand-rolled client with `selium-client` and verify the golden-path echo test stays green
