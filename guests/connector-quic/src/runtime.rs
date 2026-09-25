//! quinn `Runtime` + `AsyncTimer` over the guest executor and hostcalls.
//!
//! quinn requires a runtime abstraction to spawn its endpoint driver and to
//! create timers for loss detection and retransmit. Both are built on the
//! guest's cooperative executor and the hostcall clock:
//!
//! - `spawn` bridges quinn's `Send`-bound driver future onto the
//!   single-threaded guest reactor.
//! - `new_timer` produces a timer that sleeps via the `Sleep` hostcall
//!   (through [`selium_guest::Timer`]) and reads `now` from the hostcall
//!   monotonic clock.
//!
//! `web_time::Instant` is used as the clock type to match quinn's own
//! instant type on every target: on native it is `std::time::Instant`, and on
//! `wasm32-unknown-unknown` it is the connector-registered custom time source
//! (see [`crate::register_wasm_time_source`]).

use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};
// Only used by `wrap_udp_socket`, which quinn's trait defines on non-wasm
// targets only.
#[cfg(not(target_arch = "wasm32"))]
use std::{io, sync::Arc};

use quinn::{AsyncTimer, Runtime};
// Only used by `wrap_udp_socket`, which quinn's trait defines on non-wasm
// targets only.
#[cfg(not(target_arch = "wasm32"))]
use quinn::AsyncUdpSocket;

/// quinn `Runtime` implementation for the connector guest.
#[derive(Debug, Default)]
pub struct ConnectorRuntime;

/// quinn `AsyncTimer` implementation sleeping via the guest `Sleep` hostcall.
pub struct ConnectorTimer {
    deadline: web_time::Instant,
    sleep: Option<selium_guest::Timer>,
}

impl Runtime for ConnectorRuntime {
    fn new_timer(&self, deadline: web_time::Instant) -> Pin<Box<dyn AsyncTimer>> {
        Box::pin(ConnectorTimer::new(deadline))
    }

    fn spawn(&self, future: Pin<Box<dyn Future<Output = ()> + Send>>) {
        // quinn's driver futures are `Send`; the guest executor's `spawn`
        // accepts them directly — task capsules migrate between workers, so
        // no `Send` bound is dropped here.
        selium_guest::spawn(future);
    }

    // quinn's `Runtime::wrap_udp_socket` only exists on non-wasm targets (its
    // `wasm_browser` cfg excludes it there), so mirror that gating here: the
    // connector always uses `Endpoint::new_with_abstract_socket` on wasm32.
    #[cfg(not(target_arch = "wasm32"))]
    fn wrap_udp_socket(&self, _: std::net::UdpSocket) -> io::Result<Arc<dyn AsyncUdpSocket>> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "use Endpoint::new_with_abstract_socket with QuicUdpSocket",
        ))
    }

    fn now(&self) -> web_time::Instant {
        web_time::Instant::now()
    }
}

impl ConnectorTimer {
    /// Creates a timer that expires at `deadline`.
    pub fn new(deadline: web_time::Instant) -> Self {
        Self {
            deadline,
            sleep: None,
        }
    }
}

impl AsyncTimer for ConnectorTimer {
    fn reset(self: Pin<&mut Self>, deadline: web_time::Instant) {
        let this = self.get_mut();
        this.deadline = deadline;
        this.sleep = None;
    }

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        let this = self.get_mut();

        if web_time::Instant::now() >= this.deadline {
            this.sleep = None;
            return Poll::Ready(());
        }

        if this.sleep.is_none() {
            let remaining = this
                .deadline
                .saturating_duration_since(web_time::Instant::now());
            let deadline = match selium_guest::Instant::now() {
                Ok(now) => now
                    .checked_add(remaining)
                    .unwrap_or(selium_guest::Instant::MAX),
                Err(_) => selium_guest::Instant::MAX,
            };
            this.sleep = Some(selium_guest::Timer::new(deadline));
        }

        if let Some(sleep) = this.sleep.as_mut() {
            match Pin::new(sleep).poll(cx) {
                Poll::Ready(()) => {
                    this.sleep = None;
                    Poll::Ready(())
                }
                Poll::Pending => Poll::Pending,
            }
        } else {
            Poll::Pending
        }
    }
}

impl std::fmt::Debug for ConnectorTimer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConnectorTimer")
            .field("deadline", &self.deadline)
            .finish()
    }
}
