## Context

The existing TCP module integrates with Axum by gating `impl axum::serve::Listener for TcpListener` behind `#[cfg(feature = "axum")]` in a `mod axum_impl` block within `tcp.rs`. This keeps the optional integration fully co-located with the type it implements the trait for. The UDP module (`net/udp.rs`) follows the same crate structure. Quinn integration follows this exact pattern.

The UDP module provides `UdpSocket` with `send_to` / `recv_from` methods over shared-memory channels. Quinn needs lower-level access — it drives its own I/O loop via `AsyncUdpSocket::poll_recv` and `UdpSender::poll_send`. The integration layer bridges these two levels.

## Goals / Non-Goals

**Goals:**
- Implement `quinn::AsyncUdpSocket`, `quinn::UdpSender`, `quinn::Runtime`, and `quinn::AsyncTimer` for the guest's `UdpSocket`
- Follow the exact same structural pattern as `mod axum_impl` in `tcp.rs`: optional feature, `mod quinn_impl` inside `udp.rs`
- Enable guest code to construct a `quinn::Endpoint` via `Endpoint::new_with_abstract_socket(config, server_config, Box::new(socket), runtime)`
- Add the `quinn` workspace dependency

**Non-Goals:**
- Server-side Quinn support (incoming QUIC connections) — the `AsyncUdpSocket` trait is symmetric; both client and server use the same API
- GSO/GRO segmentation offload — single-segment mode only (`max_receive_segments = 1`, `max_transmit_segments = 1`)
- Kernel-side changes — the UDP module's proxy and channels are sufficient
- Integration tests that require a real QUIC handshake — unit tests focus on trait impl correctness

## Decisions

### 1. Feature gate and module structure (Axum pattern)

```rust
// crates/core/guest/Cargo.toml
[dependencies]
quinn = { workspace = true, optional = true }

[features]
quinn = ["dep:quinn", "io"]

// crates/core/guest/src/net/udp.rs
#[cfg(feature = "quinn")]
mod quinn_impl;
```

This mirrors the Axum integration exactly:
- `axum = ["dep:axum", "io", "tokio/time"]` → `quinn = ["dep:quinn", "io"]`
- `#[cfg(feature = "axum")] mod axum_impl { ... }` → `#[cfg(feature = "quinn")] mod quinn_impl { ... }`
- Both are unconditionally discoverable in `udp.rs` — the feature gate is the only conditional

### 2. `UdpSocket` needs `Send + Sync` inner state for Quinn

Quinn's `AsyncUdpSocket` bound is `Send + Sync + Debug + 'static`. Internally, Quinn clones the sender via `create_sender(&self)` which takes a shared reference. The `UdpSender` also needs shared access to the send channel.

**Approach:** Inside `mod quinn_impl`, define a `QuinnUdpSocket` wrapper:

```rust
#[cfg(feature = "quinn")]
mod quinn_impl {
    use std::sync::Arc;

    struct UdpSocketInner {
        recv_reader: super::StrongReader,
        recv_signal: super::Signal,
        send_writer: super::StrongWriter,
        send_signal: super::Signal,
        local_addr: std::net::SocketAddr,
    }

    // Safety: the guest is single-threaded; shared-memory operations are
    // atomic at the channel-frame level.
    unsafe impl Send for UdpSocketInner {}
    unsafe impl Sync for UdpSocketInner {}

    pub(crate) struct QuinnUdpSocket(Arc<UdpSocketInner>);
}
```

The `QuinnUdpSocket` is constructed from an owned `UdpSocket` by extracting its inner channels into the `Arc`. This avoids changing `UdpSocket`'s non-Quinn API.

**Alternatives considered:**
- Making `UdpSocket` itself `Arc<UdpSocketInner>` unconditionally — adds `Arc` overhead to non-Quinn users
- Using `unsafe impl Send + Sync for UdpSocket` directly — riskier since `UdpSocket` has a public API that isn't designed for shared ownership

### 3. `AsyncUdpSocket` implementation

```rust
impl quinn::AsyncUdpSocket for QuinnUdpSocket {
    fn create_sender(&self) -> Pin<Box<dyn quinn::UdpSender>> {
        Box::pin(QuinnUdpSender {
            inner: self.0.clone(),
            pending_signal: None,
        })
    }

    fn poll_recv(
        &mut self,
        cx: &mut Context<'_>,
        bufs: &mut [IoSliceMut<'_>],
        meta: &mut [RecvMeta],
    ) -> Poll<io::Result<usize>> {
        // 1. Try to read a frame from the recv channel
        // 2. If data available: copy payload into bufs[0], fill meta[0] with
        //    source addr + len + stride, return Ready(1)
        // 3. If channel empty: start SignalWait on recv_signal, return Pending
    }

    fn local_addr(&self) -> io::Result<SocketAddr> {
        Ok(self.0.local_addr)
    }

    fn max_receive_segments(&self) -> usize { 1 }
    fn may_fragment(&self) -> bool { false }
}
```

### 4. `UdpSender` implementation

Quinn's `UdpSender` is not tied to `AsyncUdpSocket` — `create_sender` returns any type implementing `UdpSender`. We implement it directly (not via `UdpSenderHelper`, which is not public):

```rust
struct QuinnUdpSender {
    inner: Arc<UdpSocketInner>,
    pending_signal: Option<HostcallFuture>,
}

impl quinn::UdpSender for QuinnUdpSender {
    fn poll_send(
        self: Pin<&mut Self>,
        transmit: &Transmit<'_>,
        cx: &mut Context<'_>,
    ) -> Poll<io::Result<()>> {
        // 1. If pending_signal exists, poll it for completion
        // 2. Write transmit.contents + transmit.destination to send channel
        // 3. If channel full: start SignalWait on send_signal, return Pending
        // 4. If success: return Ready(Ok(()))
    }

    fn max_transmit_segments(&self) -> usize { 1 }
}
```

### 5. `Runtime` implementation

Quinn needs a `Runtime` to spawn the `EndpointDriver` and create timers. The `SeliumQuinnRuntime` is a stateless unit struct:

```rust
#[derive(Debug)]
struct SeliumQuinnRuntime;

impl quinn::Runtime for SeliumQuinnRuntime {
    fn spawn(&self, future: Pin<Box<dyn Future<Output = ()> + Send>>) {
        // Bridge Send-bound future to the guest's single-threaded runtime.
        // Safety: the guest runtime is single-threaded; Send-bound is
        // asserted for trait compatibility only.
        let fut: Pin<Box<dyn Future<Output = ()>>> = unsafe {
            std::mem::transmute(future)
        };
        crate::async_runtime::spawn(fut);
    }

    fn new_timer(&self, deadline: Instant) -> Pin<Box<dyn AsyncTimer>> {
        Box::pin(SeliumTimer { deadline })
    }

    fn wrap_udp_socket(&self, _: std::net::UdpSocket) -> io::Result<Box<dyn AsyncUdpSocket>> {
        Err(io::Error::new(io::ErrorKind::Unsupported,
            "use new_with_abstract_socket for QuinnUdpSocket"))
    }

    fn now(&self) -> Instant {
        std::time::Instant::now()
    }
}
```

### 6. `AsyncTimer` implementation

For deadline-based wakeups, we cannot use `tokio::time::Sleep` inside the guest's cooperative runtime (there's no tokio runtime active). Instead, we use a signal-based approach:

```rust
struct SeliumTimer {
    deadline: Instant,
}

impl quinn::AsyncTimer for SeliumTimer {
    fn reset(self: Pin<&mut Self>, deadline: Instant) {
        self.get_mut().deadline = deadline;
    }

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        let deadline = self.deadline;
        if Instant::now() >= deadline {
            return Poll::Ready(());
        }
        // Register waker to be called when deadline arrives.
        // For the initial implementation, spawn a short background
        // signal-wait with the remaining duration as timeout.
        let remaining = deadline.saturating_duration_since(Instant::now());
        // Use SignalWait with timeout_ms as a general-purpose sleep.
        // The hostcall completes when the timeout fires.
        // ...
        Poll::Pending
    }
}
```

**Open question:** The guest has no standalone sleep/timer hostcall. Options for the initial impl:
1. **(Simplest)** Use a signal with a dedicated timeout-based wait — but signals are tied to shared resources
2. **(Workable)** Spawn a brief background polling loop that yields — not precise but functional
3. **(Recommended for initial impl)** Use `std::thread::sleep` in a helper OS thread + `wake_task` — similar to how kernel proxies work. The thread sleeps for the remaining duration, then invokes the task's waker.

Option 3 is the most reliable for correctness. The `poll` implementation stores the waker and spawns a thread:

```rust
fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
    if Instant::now() >= self.deadline {
        return Poll::Ready(());
    }
    let waker = cx.waker().clone();
    let deadline = self.deadline;
    std::thread::spawn(move || {
        let duration = deadline.saturating_duration_since(Instant::now());
        std::thread::sleep(duration);
        waker.wake();
    });
    Poll::Pending
}
```

This is not efficient but is correct for the initial implementation. A signal-based timer hostcall can replace it in a follow-up.

### 7. Public API surface

The Quinn integration types are not directly exposed — users interact with them indirectly through `quinn::Endpoint::new_with_abstract_socket`:

```rust
// Guest code
use selium_guest::net::udp::UdpSocket;

let socket = UdpSocket::bind("0.0.0.0:0").await?;
let runtime = Arc::new(selium_guest::net::udp::SeliumQuinnRuntime);
let endpoint = quinn::Endpoint::new_with_abstract_socket(
    EndpointConfig::default(),
    None,
    Box::new(socket.into_quinn_socket()),
    runtime,
)?;
```

The `UdpSocket` gains a `#[cfg(feature = "quinn")] fn into_quinn_socket(self) -> QuinnUdpSocket` method that converts the owned `UdpSocket` into the `Arc`-wrapped Quinn-compatible wrapper.

## Risks / Trade-offs

| Risk | Mitigation |
|------|------------|
| `unsafe impl Send + Sync for UdpSocketInner` is required | Standard for shared-memory I/O types in single-threaded runtimes; document safety invariants clearly |
| The `spawn` transmute from `Send` to non-`Send` is technically UB if the future captures non-Send types | The guest runtime is single-threaded so this is sound in practice; use a SAFETY comment explaining the invariant |
| Timer accuracy is limited by OS thread wake latency with the `std::thread::sleep` approach | Acceptable for initial implementation; QUIC retransmit timers are in the tens-to-hundreds of milliseconds range |
| The `EndpointDriver` future holds a `std::sync::Mutex` which can panic if the same thread locks it re-entrantly | Quinn does not re-enter its own lock in the driver loop; this is safe in single-threaded usage |
| No dedicated sleep hostcall means timer polling may busy-wake the guest reactor | The OS-thread approach avoids busy-waking — the reactor only polls when the thread wakes the task |
