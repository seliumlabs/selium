## 1. Workspace & Feature Setup

- [x] 1.1 Add `quinn` workspace dependency to root `Cargo.toml`
- [x] 1.2 Add optional `quinn` dependency and `quinn` feature to `crates/core/guest/Cargo.toml` (mirroring `axum` feature pattern: `quinn = ["dep:quinn", "io"]`)

## 2. Inner State & Send+Sync Safety

- [x] 2.1 Define `UdpSocketInner` struct inside `mod quinn_impl` with `Arc`-wrapped channels, signals, and local_addr
- [x] 2.2 Add `unsafe impl Send for UdpSocketInner` and `unsafe impl Sync for UdpSocketInner` with SAFETY comments documenting single-threaded invariant
- [x] 2.3 Define `QuinnUdpSocket(Arc<UdpSocketInner>)` wrapper struct with `Debug` impl
- [x] 2.4 Add `#[cfg(feature = "quinn")] fn into_quinn_socket(self) -> QuinnUdpSocket` on `UdpSocket` that extracts the inner channels into the `Arc`-wrapped state

## 3. AsyncUdpSocket Implementation

- [x] 3.1 Implement `quinn::AsyncUdpSocket for QuinnUdpSocket` with `create_io_poller` that returns a boxed `QuinnUdpPoller`
- [x] 3.2 Implement `poll_recv` that reads a frame from the recv channel, copies payload into `bufs[0]`, and populates `meta[0]` with source address and length
- [x] 3.3 Implement `poll_recv` empty-channel path that starts a `SignalWait` hostcall and returns `Poll::Pending`
- [x] 3.4 Implement `local_addr`, `max_receive_segments` (return 1), and `may_fragment` (return false)

## 4. UdpSender Implementation

- [x] 4.1 Define `QuinnUdpPoller` struct holding `Arc<UdpSocketInner>` and `pending_signal: Option<HostcallFuture>`
- [x] 4.2 Implement `quinn::UdpPoller for QuinnUdpPoller` with `poll_writable` that waits on the send signal
- [x] 4.3 Implement `poll_writable` full-channel path that starts a `SignalWait` hostcall on the send signal and returns `Poll::Pending`
- [x] 4.4 `max_transmit_segments` (return 1) is implemented via the `AsyncUdpSocket` trait default

## 5. Runtime Implementation

- [x] 5.1 Define `SeliumQuinnRuntime` unit struct with `#[derive(Debug)]`
- [x] 5.2 Implement `quinn::Runtime for SeliumQuinnRuntime` with `spawn` that bridges `Send`-bound future to the guest's `async_runtime::spawn` via unsafe transmute
- [x] 5.3 Implement `now` returning `std::time::Instant::now()`
- [x] 5.4 Implement `wrap_udp_socket` returning `Unsupported` error (unused with `new_with_abstract_socket`)

## 6. AsyncTimer Implementation

- [x] 6.1 Define `SeliumTimer` struct with `deadline: Instant`
- [x] 6.2 Implement `quinn::AsyncTimer for SeliumTimer` with `reset` and `poll`
- [x] 6.3 Implement the deadline-wakeup mechanism (OS thread with `std::thread::sleep` + `waker.wake()`)
- [x] 6.4 Wire `SeliumTimer` into `SeliumQuinnRuntime::new_timer`

## 7. Module Wiring

- [x] 7.1 Add `#[cfg(feature = "quinn")] mod quinn_impl;` inside `crates/core/guest/src/net/udp.rs`
- [x] 7.2 Re-export `SeliumQuinnRuntime` from `mod quinn_impl` so it's accessible as `selium_guest::net::udp::SeliumQuinnRuntime`
- [x] 7.3 Verify the module compiles with `cargo check --features quinn`

## 8. Tests

- [x] 8.1 Add `#[cfg(feature = "quinn")]` unit test that verifies `QuinnUdpSocket` implements `quinn::AsyncUdpSocket`
- [x] 8.2 Add unit test that `SeliumQuinnRuntime` satisfies the `quinn::Runtime` trait bounds (type-check test, like the Axum assert_listener test)
- [x] 8.3 Verify `into_quinn_socket` correctly transfers channel ownership (type-check test)
