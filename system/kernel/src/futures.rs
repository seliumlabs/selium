use std::{sync::Arc, task::Waker};

use parking_lot::Mutex;

struct FutureSharedInner<Output> {
    result: Option<Output>,
    waker: Option<Waker>,
    dropped: bool,
}

/// Shared state backing a guest-visible future.
pub struct FutureSharedState<Output> {
    inner: Mutex<FutureSharedInner<Output>>,
}

impl<Output> FutureSharedInner<Output> {
    pub fn new() -> Self {
        Self {
            result: None,
            waker: None,
            dropped: false,
        }
    }
}

impl<Output> FutureSharedState<Output> {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(FutureSharedInner::new()),
        })
    }

    /// Store the completion result and wake any registered guest task.
    pub fn resolve(self: &Arc<Self>, result: Output) {
        let mut inner = self.inner.lock();
        if inner.dropped {
            return;
        }

        inner.result = Some(result);
        if let Some(waker) = inner.waker.take() {
            waker.wake();
        }
    }

    /// Register a waker for the guest task awaiting this future.
    pub fn register_waker(self: &Arc<Self>, waker: Waker) {
        let mut inner = self.inner.lock();
        if inner.dropped {
            return;
        }

        inner.waker = Some(waker);
        if inner.result.is_some()
            && let Some(waker) = inner.waker.take()
        {
            waker.wake();
        }
    }

    /// Retrieve the completion result, if available
    pub fn take_result(self: &Arc<Self>) -> Option<Output> {
        let mut inner = self.inner.lock();
        inner.result.take()
    }

    /// Mark the future as dropped by the guest; subsequent completions are ignored.
    pub fn abandon(self: &Arc<Self>) {
        let mut inner = self.inner.lock();
        inner.dropped = true;
        inner.result = None;
        inner.waker = None;
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    use futures_util::task::{ArcWake, waker_ref};

    use super::*;
    use crate::guest_data::GuestResult;

    struct FlagWaker {
        flag: Arc<AtomicBool>,
    }

    impl ArcWake for FlagWaker {
        fn wake_by_ref(arc_self: &Arc<Self>) {
            arc_self.flag.store(true, Ordering::SeqCst);
        }
    }

    #[test]
    fn resolve_notifies_registered_waker() {
        let state = FutureSharedState::<GuestResult<Vec<u8>>>::new();
        let flag = Arc::new(AtomicBool::new(false));
        let waker = waker_ref(&Arc::new(FlagWaker { flag: flag.clone() })).clone();

        state.register_waker(waker);
        state.resolve(Ok(Vec::new()));

        assert!(flag.load(Ordering::SeqCst));
        assert!(state.take_result().is_some());
    }
}
