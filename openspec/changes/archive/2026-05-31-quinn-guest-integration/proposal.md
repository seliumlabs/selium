## Why

The UDP module provides raw datagram send/receive, but QUIC support requires implementing Quinn's `AsyncUdpSocket` and `Runtime` traits so that `quinn::Endpoint::new_with_abstract_socket` can drive the connection state machine over our shared-memory channels. This unlocks QUIC-based communication for guest processes — the same relationship as Axum over TCP.

## What Changes

- **New `quinn` feature** on `selium-guest` (optional, gated with `#[cfg(feature = "quinn")]`), mirroring the existing `axum` feature pattern
- **`mod quinn_impl`** inside `crates/core/guest/src/net/udp.rs` containing Quinn trait implementations, following the same convention as `mod axum_impl` in `tcp.rs`
- **`impl quinn::AsyncUdpSocket for UdpSocket`** — maps `poll_recv`, `create_sender`, `local_addr` to the shared-memory channels
- **Custom `UdpSender` impl** — `poll_send` writes frames to the send channel, waits on signal when full
- **`SeliumQuinnRuntime`** — implements `quinn::Runtime` to bridge the guest's async runtime with Quinn's `spawn` and `new_timer` requirements
- **`SeliumTimer`** — implements `quinn::AsyncTimer` for deadline-based wakeups using guest timeout primitives
- **New workspace dependency** on the `quinn` crate (client-side API: `quinn::Endpoint`, `quinn::AsyncUdpSocket`, `quinn::UdpSender`, etc.)

## Capabilities

### New Capabilities
- `quinn-transport`: covers the Quinn trait implementations (`AsyncUdpSocket`, `UdpSender`, `Runtime`, `AsyncTimer`) that enable constructing a `quinn::Endpoint` from a guest's `UdpSocket`.

### Modified Capabilities
- `selium-guest`: requires new optional `quinn` feature, re-export of Quinn integration types, and the `mod quinn_impl` within `net/udp.rs`

## Impact

| Area | Impact |
|------|--------|
| `selium-guest` | New `quinn` feature in `Cargo.toml`; new `mod quinn_impl` in `net/udp.rs` implementing `quinn::AsyncUdpSocket`, `quinn::UdpSender`, `quinn::Runtime`, `quinn::AsyncTimer`; public re-exports under the feature gate |
| `selium-guest`'s `UdpSocket` | Internal channels (`StrongReader`/`StrongWriter`/`Signal`) become `Send + Sync` via `unsafe impl` so the `AsyncUdpSocket` bound is satisfied; optional `Arc`-wrapped inner state for the Quinn path |
| Workspace `Cargo.toml` | New `quinn` workspace dependency added (path or crates.io) |
| Axum / TCP | Unaffected |
| `add-udp-module` change | Builds on top of it — the UDP module must be implemented first |
