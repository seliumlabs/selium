## Context

The "external client" exists today only as hand-rolled quinn code in tests: `quic_spine` (endpoint, TLS config, stream round-trips) and the connector's native handshake tests (client cert config). The bridge protocol is already specified (`guest-bridge`) and implemented: the connector terminates QUIC, the bridge-server authenticates via handoff `ClientIdentity`, and a bridge-channel splices `selium-wire` frames between one QUIC stream and one fabric channel after a typed `PipeControl::Handshake { uri }`. `selium-wire`'s handles are generic over `MessageTransport`; its read path assumes a generation counter and falls back to a cooperative `yield_now()` loop (fine in a guest reactor, a spin on native tokio).

## Goals / Non-Goals

**Goals:**
- One native, publishable crate that collapses quinn + TLS + mTLS + handshake + framing behind `connect` and typed channel handles.
- A deterministic bridge handshake: every channel open awaits a typed reply, so attach failures surface at open for every handle type.
- A waker-honest `selium-wire` read path so native consumers park on the socket.

**Non-Goals:**
- Provisioning/issuing client identity certs (accepted, never generated).
- No protocol changes beyond the deterministic handshake reply (one additive control frame); the relay and teardown semantics are untouched.
- A JS/WASM build of the client; this is host-side Rust.

## Decisions

### 1. Native crate at `crates/client`, not a guest
The client runs on users' hosts and speaks QUIC directly, so it cannot be a WASM guest (no sockets; guests reach the fabric via channels). Chosen over a guest-based proxy, which would defeat the point of an external SDK.

### 2. `selium-wire` gains a waker-honest read path, selected by transport
The spin is in `FramedRead::poll_ready` (no-op waker) and the `yield_now()` fallback in `pubsub`/`rpc`/`stream`/`tables`. Rather than a compile-time feature (two crates in one workspace — the native client and the guest SDK — share `selium-wire`, so a mutually-exclusive feature would break `cargo --workspace` builds), the read path is selected at runtime per transport: generation-tracked transports (`region_id != 0`) keep the generation/yield semantics unchanged, while transports without a generation counter poll the underlying `AsyncRead` with the caller's waker and park on the socket. Chosen over (a) reimplementing framing in the client — duplicates the codec; and (b) changing the default ring behaviour — would risk the guest reactor and the ring fast path.

### 3. The client owns its `MessageTransport` adapter; `selium-wire` stays quinn-free
A read/`write` pair adapts `quinn::RecvStream`/`SendStream`, mirroring the bridge-channel's `StreamRead/WriteTransport` (`poll_ready` = always ready, `generation() == 0`, peer-closed from stream state). This keeps quinn out of `selium-wire`'s dependency graph.

### 4. Relocate `PipeControl` from `selium-bridge-channel` into `selium-wire`
The handshake and termination types are the shared contract between the client and the bridge-channel. Moving them to `selium-wire` (which bridge-channel already depends on) avoids a duplicated copy and a client dependency on a guest crate. The bridge-channel imports it back with no wire change.

### 5. API shape: `connect` + one-stream-per-channel, typed with hidden transport
`client::connect(addr, ConnectOptions { server_root, identity })` returns a `Client` owning the `quinn::Connection`. Channel methods (`publisher::<T>(uri)`, `subscriber::<T>(uri)`, `rpc::<Req, Rep>(uri)`, streaming, live-table) open a fresh bidi stream and return handles whose transport parameter is pinned (thin newtypes over the wire handles) while the message type stays generic. Subscriber is `futures::Stream`, publisher is `futures::Sink<T>`, and every handle maps its errors onto the crate error type.

### 6. Errors collapse into one crate error
A single `Error` enum wraps quinn/rustls/io errors. Underlying `selium-wire` errors pass through a `Wire` variant (with a `From` that enriches serialization failures); `RpcError` maps through a `From` that keeps remote stream errors, serialization, and closed-connection semantics as dedicated variants. Termination codes arrive at channel open via the deterministic reply and map to `BadHandshake`/`AttachFailed`/`Terminated(code)`. `selium_wire::Error` itself stays untouched by client concerns — the code mapping lives where the codes are observed (the client, at open).

### 7. The handshake reply is deterministic: `Accepted` on success
The bridge previously replied only on failure — silence meant the splice had begun — so an open-time error was indistinguishable from a quiet channel, and a write-only handle could never observe the refusal. The bridge-channel now sends one additive control frame, `PipeControl::Accepted`, after resolve/attach succeeds and immediately before the splice; `Terminate` remains the refusal reply. The client awaits the reply at channel open and maps it to typed errors, for every handle type. The reply is read through the framed reader the handle itself keeps: the codec may read past the reply into relayed data frames, and rebuilding the reader would silently discard them. Chosen over (a) a bounded probe window at open (racy — a late termination slips into the data path — and taxes every open) and (b) first-frame screening in the read path (write-only handles never read, so publishers could not surface refusals at all).

## Risks / Trade-offs

- [Waker-honest reads touch shared code] → Selected per transport (`region_id == 0` vs not); generation-tracked reads are unchanged, so the ring fast path and guest reactor are untouched. Run the existing ring tests to prove no regression.
- [`LiveTable` is the least async-native overlay] → Non-ring transports get an async drive path: `sync_async` parks on the transport's read waker, and `set_async`/`delete_async` park until the table's own mutation is replayed.
- [Transport generics leak into handle type names] → Handled as thin newtypes over the wire handles, pinning `M` = the quinn transport so user signatures stay `Subscriber<T>` while errors surface as the crate error.
- [Multi-publisher tag collision] → `Publisher` defaults to writer id `0`; `set_writer_id` is exposed and distinct ids are documented for multiple publishers on one topic.
- [Every channel open now awaits a bridge reply] → Adds one round trip per open, bounded by the bridge's local resolve/attach (the reply is sent before the relay begins). The client and bridge land together in this change; a rollback reverts both sides.

## Migration Plan

No migration — a new crate plus transport-selected wire changes plus one additive bridge control frame. Sequence: waker-honest wire read path first (independently testable), then the shared `PipeControl` contract with the deterministic reply (bridge-channel and client land together), then the client crate, then `quic_spine`'s hand-rolled client swap (done, on the landed `addressing` change). Rollback is a revert of the crate/read-path/bridge-reply changes; no behaviour changes land on the generation-tracked read path.

## Open Questions

- Writer-id assignment for multiple client publishers on one topic: caller-managed today (`set_writer_id`); auto-assignment remains a future convenience.
