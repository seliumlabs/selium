## Why

The current network layer uses bespoke hostcalls for every operation (`NetworkListen`, `NetworkConnect`, `NetworkStreamSend`, `NetworkStreamRecv`, etc.) and models TCP connections as kernel-managed state with per-call I/O. This means every read/write requires a hostcall round-trip, making it impossible to implement `AsyncRead`/`AsyncWrite` efficiently or integrate with ecosystem frameworks like axum. Meanwhile, the `ResourceListener`/`Accept`/`SharedRegion` infrastructure already provides a proven pattern for zero-hostcall I/O through shared-memory ring buffers with signal-based notification. Dogfooding this infrastructure for TCP eliminates 10 hostcall variants, unifies the I/O model, and enables real OS-level TCP with framework compatibility.

## What Changes

- **BREAKING**: Remove `NetworkListener`, `NetworkSession`, `NetworkStream`, and `RequestExchange` from `selium-guest`
- **BREAKING**: Remove `NetworkListen`, `NetworkListenerClose`, `NetworkConnect`, `NetworkSessionClose`, `NetworkOpenStream`, `NetworkStreamClose`, `NetworkStreamSend`, `NetworkStreamRecv`, `NetworkSendRequest`, `NetworkWaitRequestResponse` from `HostcallRequest`/``HostcallOutput``
- **BREAKING**: Remove `Listener`, `Session`, `Stream`, `RequestExchange` from `ResourceClass`
- **BREAKING**: Remove `NetworkListenerDescriptor`, `NetworkSessionDescriptor`, `NetworkStreamDescriptor` from `selium-abi`
- Add `TcpListener` type that wraps `ResourceListener` for inbound TCP connections
- Add `TcpStream` type that attaches to a shared-memory region for zero-hostcall I/O
- Add `TcpAccept` type implementing the `Accept` trait for TCP connection acceptance
- Add `TcpBind` and `TcpConnect` hostcall variants to `HostcallRequest`/`HostcallOutput`
- Add kernel-side network runtime that binds real TCP sockets, creates shared-memory regions with ring buffers and signals, and proxies bytes between real sockets and shared memory
- Add kernel-side ring buffer primitives for the proxy to read/write shared memory
- Implement `tokio::io::AsyncRead` and `tokio::io::AsyncWrite` for `TcpStream`
- Add optional `axum` feature with `impl axum::serve::Listener for TcpListener`
- Remove kernel's `ListenerState`, `SessionState`, `StreamState`, `RequestExchangeState` and all network hostcall handlers

## Capabilities

### New Capabilities
- `tcp-listener`: Inbound TCP connection listening via ResourceListener, TcpAccept, and shared-memory stream attachment
- `tcp-stream`: Zero-hostcall TCP stream I/O through shared-memory ring buffers with AsyncRead/AsyncWrite implementations
- `network-runtime`: Kernel-side TCP proxy that bridges real OS sockets to shared-memory ring buffers using signal notification

### Modified Capabilities
- `hostcall-abi`: Replace 10 network hostcall variants with `TcpBind` and `TcpConnect`; remove `Listener`, `Session`, `Stream`, `RequestExchange` resource classes and descriptors from ABI types

## Impact

- **selium-abi**: Breaking changes to `HostcallRequest`, `HostcallOutput`, `ResourceClass`, and descriptor types
- **selium-guest**: New `TcpListener`, `TcpStream`, `TcpAccept` types; removal of `NetworkListener`, `NetworkSession`, `NetworkStream`, `RequestExchange`; new `axum` feature flag
- **selium-kernel**: New `network` module with TCP proxy runtime; removal of existing `network.rs` stubs; new ring buffer primitives for shared memory I/O; new hostcall handlers for `TcpBind` and `TcpConnect`
- **selium-guest-macros**: No changes expected
- **Runtime**: New async task spawning for TCP proxy per connection; HostQueue integration for listener accept loop
- **Guest applications**: Must migrate from `Network*` types to `TcpListener`/`TcpStream`; can now use axum and other tokio-based frameworks directly