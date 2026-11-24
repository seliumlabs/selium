//! Cancellation-safe single-session client ownership.
//!
//! Store and control clients own exactly one shared-memory RPC session and
//! serialise requests cooperatively. Checkout must survive cancellation at
//! every await point: dropping a request future may not leave the gate closed
//! forever or strand the session outside its owner.

use std::sync::{
    Mutex,
    atomic::{AtomicBool, Ordering},
};

use crate::async_runtime::yield_now;

/// The shared state behind a one-session client, generic over the session
/// handle so the checkout contract can be tested without a live transport.
pub(crate) struct SessionState<S> {
    gate: AtomicBool,
    session: Mutex<Option<S>>,
}

/// RAII checkout of a client's session.
///
/// On normal completion the caller restores the session and the guard only
/// reopens the gate. If the future is dropped first (cancellation, early
/// return, or a transport failure), dropping the guard closes the session and
/// still reopens the gate, so later requests are admitted and observe a clear
/// "session unavailable" error instead of spinning on a stranded gate.
pub(crate) struct SessionGuard<'a, S> {
    state: &'a SessionState<S>,
    session: Option<S>,
    restored: bool,
}

impl<S> SessionState<S> {
    /// Creates the shared state around an established session.
    pub(crate) fn new(session: S) -> Self {
        Self {
            gate: AtomicBool::new(false),
            session: Mutex::new(Some(session)),
        }
    }

    /// Takes exclusive ownership of the session, yielding until the gate opens.
    pub(crate) async fn acquire(&self) -> SessionGuard<'_, S> {
        while self.gate.swap(true, Ordering::AcqRel) {
            yield_now().await;
        }
        let session = self
            .session
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        SessionGuard {
            state: self,
            session,
            restored: false,
        }
    }
}

impl<S> SessionGuard<'_, S> {
    /// Returns the checked-out session, if it is still usable.
    pub(crate) fn session(&mut self) -> Option<&mut S> {
        self.session.as_mut()
    }

    /// Marks a normally completed request and returns the session to the
    /// client before the guard reopens the gate.
    pub(crate) fn restore(&mut self) {
        if let Some(session) = self.session.take() {
            *self
                .state
                .session
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(session);
        }
        self.restored = true;
    }
}

impl<S> Drop for SessionGuard<'_, S> {
    fn drop(&mut self) {
        if !self.restored {
            // Cancelled or failed mid-request: the session may hold a partial
            // frame or a pending reply. Close it rather than reuse a torn
            // transport; `self.session` is dropped here.
            drop(self.session.take());
        }
        self.state.gate.store(false, Ordering::Release);
    }
}

#[cfg(test)]
pub(crate) async fn test_session_pair<Req, Rep>() -> (
    selium_shm::rpc::OwnedRpcClient<Req, Rep>,
    tokio::task::JoinHandle<()>,
)
where
    Req: selium_service::FlatMsg + Send + 'static,
    Rep: selium_service::FlatMsg + Send + 'static,
{
    drop(selium_memory::set_region_provider(Box::new(
        selium_memory::HeapRegionProvider::new(),
    )));
    use selium_wire::rpc::Rendezvous as _;
    let rendezvous = selium_shm::ShmRendezvous::new();
    let server = {
        let rendezvous = rendezvous.clone();
        tokio::spawn(async move {
            let incoming = rendezvous.recv().await.expect("incoming session");
            let _connection: selium_shm::rpc::RpcConnection<Req, Rep> =
                selium_shm::rpc::accept(incoming).expect("accept session");
            // Keep the peer connection alive without replying, so a client
            // request parks where it can be cancelled.
            std::future::pending::<()>().await;
        })
    };
    let session = selium_shm::rpc::connect::<Req, Rep, _>(rendezvous, 0, 0)
        .await
        .expect("connect session");
    (session, server)
}

#[cfg(test)]
mod tests {
    use std::{
        future::Future,
        pin::Pin,
        sync::Arc,
        task::{Context, Poll},
    };

    use super::*;

    async fn checkout_then_wait(state: &SessionState<u32>) {
        let mut guard = state.acquire().await;
        assert!(guard.session().is_some());
        // Stand-in for `session.request(...).await`: this await point is where
        // a caller can drop the future.
        std::future::pending::<()>().await;
        guard.restore();
    }

    fn poll_once<F: Future>(future: Pin<&mut F>) -> Poll<F::Output> {
        let waker = std::task::Waker::noop();
        let mut context = Context::from_waker(waker);
        future.poll(&mut context)
    }

    #[test]
    fn cancellation_mid_request_closes_the_session_and_reopens_the_gate() {
        let state = SessionState::new(7u32);
        let mut request = Box::pin(checkout_then_wait(&state));
        assert!(poll_once(request.as_mut()).is_pending());

        // Cancel at the await point.
        drop(request);

        let state = Arc::new(state);
        let mut next = Box::pin(state.acquire());
        let mut guard = match poll_once(next.as_mut()) {
            Poll::Ready(guard) => guard,
            Poll::Pending => panic!("a cancelled request left the gate stranded"),
        };
        assert!(
            guard.session().is_none(),
            "the torn session must be closed, not handed to the next caller"
        );
        guard.restore();
        drop(guard);

        // The restored (empty) state still reopens for later callers.
        let mut later = Box::pin(state.acquire());
        let mut guard = match poll_once(later.as_mut()) {
            Poll::Ready(guard) => guard,
            Poll::Pending => panic!("the gate did not reopen after the cancelled request"),
        };
        assert!(guard.session().is_none());
        guard.restore();
    }

    #[test]
    fn successful_request_restores_the_same_session() {
        let state = SessionState::new(String::from("session"));
        let mut first = Box::pin(state.acquire());
        let mut guard = match poll_once(first.as_mut()) {
            Poll::Ready(guard) => guard,
            Poll::Pending => panic!("an idle gate must acquire immediately"),
        };
        guard
            .session()
            .expect("session present")
            .push_str(":updated");
        guard.restore();
        drop(guard);

        let mut second = Box::pin(state.acquire());
        let mut guard = match poll_once(second.as_mut()) {
            Poll::Ready(guard) => guard,
            Poll::Pending => panic!("the gate did not reopen"),
        };
        assert_eq!(
            guard.session().map(|session| session.as_str()),
            Some("session:updated"),
            "a completed request must return the intact session"
        );
    }

    /// Store and control clients share this guard: repeated cancelled requests
    /// must never strand the gate, and each cancelled session stays closed.
    #[test]
    fn repeated_cancelled_store_or_control_requests_never_strand_the_gate() {
        let state = SessionState::new(1u32);
        for _ in 0..1_000 {
            {
                let mut future = Box::pin(state.acquire());
                let guard = match poll_once(future.as_mut()) {
                    Poll::Ready(guard) => guard,
                    Poll::Pending => panic!("gate stranded before cancellation"),
                };
                // Drop the guard (and the checked-out session) without restore.
                drop(guard);
            }

            let mut next = Box::pin(state.acquire());
            let mut guard = match poll_once(next.as_mut()) {
                Poll::Ready(guard) => guard,
                Poll::Pending => panic!("a cancelled request stranded the gate"),
            };
            assert!(
                guard.session().is_none(),
                "a safely closed session must not be reused"
            );
        }
    }
}
