## Why

There is no client SDK for Selium's fabric. External users must hand-roll a quinn endpoint, TLS and mTLS configuration, the bridge handshake frame, and `selium-wire` framing over raw QUIC streams — today that code exists only inside tests (`quic_spine`, the connector's native handshake tests). This change productizes that boilerplate as a single crate so an external user goes from nothing to a typed stream in a few lines.

## What Changes

- New `selium-client` crate at `crates/client/`: a native (host-side) library, not a WASM guest.
- Full SDK surface: one QUIC connection, one bidirectional stream per channel URI, typed `Publisher`/`Subscriber`/`RpcClient`/server-streaming/bidi-streaming/live-table handles with transport generics hidden and a single crate error type (wire/RPC errors pass through; termination codes, remote stream errors, and serialization map to dedicated variants).
- QUIC (TLS 1.3) and mTLS client-auth plumbing: trust the server cert, present the client identity cert/key (provisioned out-of-band; the provisioning path itself is out of scope).
- Deterministic bridge handshake: the client sends `PipeControl::Handshake { uri }` and awaits the bridge's typed reply — a new `PipeControl::Accepted` frame on success (sent after attach, before the relay begins), or `PipeControl::Terminate` on refusal — so attach failures surface as typed errors at channel open, for every handle type.
- A waker-honest async read path in `selium-wire`, selected per transport: native reads over non-ring transports park on the socket instead of the cooperative yield loop.

## Capabilities

### New Capabilities
- `selium-client`: the external client SDK — connect, channel open, typed messaging handles, mTLS identity, and error surface.

### Modified Capabilities
- `guest-bridge`: the typed pipe handshake becomes deterministic — the bridge-channel replies with an acceptance control frame on success, or a termination frame on failure.
- `transport-abstraction`: `MessageTransport` readiness over non-ring transports is waker-honest at the frame level (codec-driven; always-ready `poll_ready` for socket transports).
- `framed-io`: `FramedRead` gains a waker-honest async read path for non-ring transports.

## Impact

- Code: new `crates/client` crate; `selium-wire` transport-selected read/wait paths (`framed.rs`, `pubsub.rs`, `rpc.rs`, `stream.rs`, `tables.rs`) and the shared `PipeControl` contract (`control.rs`); the bridge-channel's deterministic handshake reply (`guests/bridge-channel`); `Cargo.toml` workspace members/deps.
- Dependencies: adds `quinn`, `rustls`/`rustls-pemfile`, `tokio` to the client.
- Depends on: the `addressing` change (its `connect(uri, addr)` ergonomics and `uri`-level name resolution). The crate's QUIC/wire core can be exercised independently against the connector's native handshake seam.
