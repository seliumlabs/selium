use core::{
    cell::RefCell,
    future::Future,
    pin::Pin,
    task::{Context, Poll, Waker},
};
use std::{
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread_local,
};

use futures::{pin_mut, task::ArcWake};

use selium_abi::GuestUint;

#[cfg(target_arch = "wasm32")]
use selium_abi::{
    GuestAtomicUint,
    mailbox::{CAPACITY, FLAG_OFFSET, HEAD_OFFSET, RING_OFFSET, SLOT_SIZE, TAIL_OFFSET},
};

#[cfg(not(target_arch = "wasm32"))]
mod host_wakers {
    use std::{
        collections::HashMap,
        sync::{Mutex, OnceLock},
        task::Waker,
    };

    use selium_abi::GuestUint;

    struct Registry {
        next: GuestUint,
        wakers: HashMap<GuestUint, Waker>,
    }

    impl Registry {
        fn new() -> Self {
            Self {
                next: 1,
                wakers: HashMap::new(),
            }
        }
    }

    fn registry() -> &'static Mutex<Registry> {
        static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();
        REGISTRY.get_or_init(|| Mutex::new(Registry::new()))
    }

    pub fn register(waker: Waker) -> GuestUint {
        let mut guard = registry().lock().expect("host waker registry poisoned");
        let id = guard.next;
        guard.next = guard.next.saturating_add(1);
        guard.wakers.insert(id, waker);
        id
    }

    #[cfg(test)]
    pub fn wake(id: GuestUint) {
        if let Ok(mut guard) = registry().lock()
            && let Some(waker) = guard.wakers.remove(&id)
        {
            waker.wake();
        }
    }
}

#[cfg(target_arch = "wasm32")]
mod host {
    #[link(wasm_import_module = "selium::async")]
    unsafe extern "C" {
        fn yield_now();
    }

    pub unsafe fn park() {
        unsafe {
            yield_now();
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
mod host {
    pub unsafe fn park() {}
}

thread_local! {
    static BACKGROUND: RefCell<Vec<BackgroundTask>> = const { RefCell::new(Vec::new()) };
    static SPAWN_QUEUE: RefCell<Vec<BackgroundTask>> = const { RefCell::new(Vec::new()) };
}

struct LocalWake {
    notified: AtomicBool,
}

impl LocalWake {
    fn new() -> Self {
        Self {
            notified: AtomicBool::new(false),
        }
    }

    fn notify(&self) {
        self.notified.store(true, Ordering::Release);
    }

    fn take_notified(&self) -> bool {
        self.notified.swap(false, Ordering::AcqRel)
    }
}

impl ArcWake for LocalWake {
    fn wake_by_ref(arc_self: &Arc<Self>) {
        arc_self.notify();
    }
}

struct BackgroundTask {
    future: Pin<Box<dyn Future<Output = ()>>>,
}

struct YieldNow {
    yielded: bool,
}

impl BackgroundTask {
    fn new<F>(future: F) -> Self
    where
        F: Future<Output = ()> + 'static,
    {
        Self {
            future: Box::pin(future),
        }
    }

    fn poll(&mut self, cx: &mut Context<'_>) -> Poll<()> {
        self.future.as_mut().poll(cx)
    }
}

struct JoinState<T> {
    result: Option<T>,
    waker: Option<Waker>,
}

impl<T> JoinState<T> {
    fn new() -> Self {
        Self {
            result: None,
            waker: None,
        }
    }

    fn complete(&mut self, value: T) {
        self.result = Some(value);
        if let Some(waker) = self.waker.take() {
            waker.wake();
        }
    }
}

/// Handle for awaiting the result of a spawned task.
pub struct JoinHandle<T> {
    state: Rc<RefCell<JoinState<T>>>,
}

impl<T> Future for JoinHandle<T> {
    type Output = T;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut state = self.state.borrow_mut();
        if let Some(value) = state.result.take() {
            Poll::Ready(value)
        } else {
            state.waker = Some(cx.waker().clone());
            Poll::Pending
        }
    }
}

impl Future for YieldNow {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        if self.yielded {
            Poll::Ready(())
        } else {
            self.yielded = true;
            cx.waker().wake_by_ref();
            Poll::Pending
        }
    }
}

#[inline(always)]
#[cfg(target_arch = "wasm32")]
unsafe fn cell(offset: usize) -> *mut GuestAtomicUint {
    offset as *mut GuestAtomicUint
}

/// Register the current [`Waker`] with the host dispatcher and return its identifier.
#[cfg(not(target_arch = "wasm32"))]
pub fn register(cx: &mut Context<'_>) -> GuestUint {
    host_wakers::register(cx.waker().clone())
}

/// Register the current [`Waker`] with the host dispatcher and return its identifier.
#[cfg(target_arch = "wasm32")]
pub fn register(cx: &mut Context<'_>) -> GuestUint {
    let waker = cx.waker().clone();
    Box::into_raw(Box::new(waker)) as GuestUint
}

#[cfg(all(test, not(target_arch = "wasm32")))]
pub fn wake(task_id: GuestUint) {
    host_wakers::wake(task_id);
}

/// Spawn a future so it can make progress alongside other guest tasks.
pub fn spawn<F>(future: F) -> JoinHandle<F::Output>
where
    F: Future + 'static,
{
    let state = Rc::new(RefCell::new(JoinState::new()));
    let state_clone = Rc::clone(&state);
    let task = async move {
        let output = future.await;
        state_clone.borrow_mut().complete(output);
    };

    let task = BackgroundTask::new(task);
    BACKGROUND.with(|tasks| {
        if let Ok(mut tasks) = tasks.try_borrow_mut() {
            tasks.push(task);
        } else {
            SPAWN_QUEUE.with(|queue| {
                queue.borrow_mut().push(task);
            });
        }
    });

    JoinHandle { state }
}

/// Yield execution to the guest scheduler once.
pub async fn yield_now() {
    YieldNow { yielded: false }.await;
}

/// Block on a future using Selium's guest-side executor.
pub fn block_on<F: Future>(fut: F) -> F::Output {
    pin_mut!(fut);
    let wake_state = Arc::new(LocalWake::new());
    let waker = futures::task::waker(Arc::clone(&wake_state));
    let mut cx = Context::from_waker(&waker);

    loop {
        if let Poll::Ready(val) = fut.as_mut().poll(&mut cx) {
            return val;
        }

        let progressed = poll_backgrounds(&mut cx);
        if progressed || wake_state.take_notified() {
            drain_mailbox();
            continue;
        }
        wait();
    }
}

fn poll_backgrounds(cx: &mut Context<'_>) -> bool {
    let mut progress = merge_spawn_queue();

    let mut skipped = false;
    BACKGROUND.with(|tasks| match tasks.try_borrow_mut() {
        Ok(mut tasks) => {
            let mut i = 0;
            while i < tasks.len() {
                match tasks[i].poll(cx) {
                    Poll::Ready(()) => {
                        tasks.swap_remove(i);
                        progress = true;
                    }
                    Poll::Pending => i += 1,
                }
            }
        }
        Err(_) => {
            skipped = true;
        }
    });

    if skipped {
        return progress;
    }

    if merge_spawn_queue() {
        progress = true;
    }

    progress
}

fn merge_spawn_queue() -> bool {
    SPAWN_QUEUE.with(|queue| {
        let mut queued = queue.borrow_mut();
        if queued.is_empty() {
            return false;
        }

        BACKGROUND.with(|tasks| {
            tasks.borrow_mut().extend(queued.drain(..));
        });

        true
    })
}

/// Wait for the host to enqueue wake IDs and dispatch them.
#[cfg(target_arch = "wasm32")]
pub fn wait() {
    unsafe {
        if (*cell(FLAG_OFFSET)).load(core::sync::atomic::Ordering::Acquire) == 0 {
            host::park();
        }
        drain();
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub fn wait() {
    unsafe {
        host::park();
    }
}

#[cfg(target_arch = "wasm32")]
fn drain_mailbox() {
    unsafe {
        if (*cell(FLAG_OFFSET)).load(core::sync::atomic::Ordering::Acquire) != 0 {
            drain();
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn drain_mailbox() {}

#[cfg(target_arch = "wasm32")]
unsafe fn drain() {
    unsafe {
        drain_ring(|id| {
            let waker = Box::from_raw(id as *mut Waker);
            waker.wake();
        });
    }
}

#[cfg(target_arch = "wasm32")]
unsafe fn drain_ring(mut schedule: impl FnMut(GuestUint)) {
    let mut head = unsafe { (*cell(HEAD_OFFSET)).load(core::sync::atomic::Ordering::Acquire) };
    let tail = unsafe { (*cell(TAIL_OFFSET)).load(core::sync::atomic::Ordering::Acquire) };

    while head != tail {
        let slot = RING_OFFSET + ((head % CAPACITY) as usize * SLOT_SIZE);
        let id = unsafe { (*cell(slot)).load(core::sync::atomic::Ordering::Relaxed) };
        schedule(id);
        head = head.wrapping_add(1);
    }

    unsafe {
        (*cell(HEAD_OFFSET)).store(head, core::sync::atomic::Ordering::Release);
    }
    unsafe {
        (*cell(FLAG_OFFSET)).store(0, core::sync::atomic::Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use futures::StreamExt;

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn register_returns_boxed_waker_off_wasm() {
        let waker = futures::task::noop_waker();
        let mut cx = std::task::Context::from_waker(&waker);
        let id = register(&mut cx);
        assert_ne!(id, 0);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn for_each_concurrent_advances_on_local_wake() {
        struct YieldOnce {
            yielded: bool,
        }

        impl Future for YieldOnce {
            type Output = ();

            fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
                if self.yielded {
                    Poll::Ready(())
                } else {
                    self.yielded = true;
                    cx.waker().wake_by_ref();
                    Poll::Pending
                }
            }
        }

        let total = 4usize;
        let counter = Arc::new(AtomicUsize::new(0));
        let counter_ref = Arc::clone(&counter);
        let fut = futures::stream::iter(0..total).for_each_concurrent(Some(2), move |_| {
            let counter = Arc::clone(&counter_ref);
            async move {
                YieldOnce { yielded: false }.await;
                counter.fetch_add(1, Ordering::Relaxed);
            }
        });

        block_on(fut);

        assert_eq!(counter.load(Ordering::Relaxed), total);
    }
}
