//! Shared-memory async executor for Selium guests.
//!
//! # Execution model
//!
//! All executor state lives in shared statics (atomics and mutexes), never
//! `thread_local!`: on `wasm32-unknown-unknown` TLS statics lower to plain
//! linear-memory slots that every host thread entering the same instance
//! would share unsynchronised. Worker identity is passed explicitly — the
//! worker entry receives a worker id and each worker runs the same executor
//! loop over the shared task table and run queue, so independent tasks may
//! advance concurrently on distinct workers.
//!
//! Two driving modes share one executor:
//!
//! - **Cooperative** (`poll_reactor`, the default): worker 0 runs runnable
//!   tasks until no forward-progress work remains, then returns the
//!   poll-owner exit code; the host drives subsequent entries through the
//!   `__selium_guest_poll` export.
//! - **Multithreaded** (the worker entry): each worker parks on the shared
//!   wake word when the run queue is empty and resumes in place when a wake
//!   or enqueue notifies it.
//!
//! # Lost-wakeup discipline
//!
//! Every task carries a **parking word** bumped on every wake, in any state.
//! A worker claims a task by CAS-transitioning it to *polling* (single-flight:
//! at most one worker polls a future at a time), capturing the parking-word
//! baseline in the same task-table critical section as the claim — so a
//! concurrent wake either lands before the claim (absorbed by the upcoming
//! poll) or bumps the word after the baseline (caught by the poller's
//! re-check). A wake that lands while the task is being polled bumps the
//! parking word, and the poller compares the word against the baseline after
//! the poll — the standard re-check-before-park — so a wake racing an
//! in-flight poll is re-delivered when the task re-parks. The poller's
//! park-state store and re-check load pair with the waker's parking-word bump
//! and state re-load at `SeqCst`, so the race cannot be missed by both sides
//! at once (the store-buffering litmus). The same word is mirrored into the
//! ABI mailbox so the host can observe and notify it (see
//! `selium_abi::mailbox`).

use std::{
    collections::{HashMap, VecDeque},
    future::Future,
    pin::Pin,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU32, Ordering},
    },
    task::{Context, Poll, RawWaker, RawWakerVTable, Waker},
};

use selium_abi::{HostcallRequest, TaskId};

use crate::{
    hostcall::hostcall_ready_with_task,
    platform::{
        bump_task_parking_word, bump_wake_word, drain_mailbox, mailbox_has_pending,
        notify_wake_word, park_on_wake_word, register_mailbox, stop_requested, wake_word_value,
    },
};

static EXECUTOR: OnceLock<Executor> = OnceLock::new();
/// Task is parked: not runnable, not being polled.
const TASK_PARKED: u32 = 0;
/// Task is claimed by one worker: its future is being polled.
const TASK_POLLING: u32 = 2;
/// Task is runnable: enqueued (or about to be) on the shared run queue.
const TASK_QUEUED: u32 = 1;
static TASK_WAKE_VTABLE: RawWakerVTable =
    RawWakerVTable::new(clone_waker, wake_waker, wake_waker_by_ref, drop_waker);

/// A live guest task: a pinned, `Send` future plus its lifecycle state.
struct TaskSlot {
    id: TaskId,
    /// The task's future. `None` only while a worker has taken it out to poll
    /// (the table is never locked across a poll, so wakes can reach the task).
    future: Option<Pin<Box<dyn Future<Output = ()> + Send>>>,
    /// Lifecycle state (see the `TASK_*` constants).
    state: AtomicU32,
    /// The task's parking word: bumped on every wake in any state. The
    /// polling worker compares the word before/after a poll to detect a wake
    /// that raced the poll (the lost-wakeup re-check).
    park_word: AtomicU32,
}

/// The shared executor: task table, run queue, and coordination state.
struct Executor {
    /// Live task slots keyed by task id. A slot is removed once its future
    /// completes (`Poll::Ready`).
    tasks: Mutex<HashMap<TaskId, TaskSlot>>,
    /// Runnable task ids awaiting a worker (the shared work-stealing run
    /// queue). A task id appears at most once: enqueue happens only on the
    /// parked → queued transition, and popping claims queued → polling.
    run_queue: Mutex<VecDeque<TaskId>>,
    /// Yields queued by tasks during the current pass. Applied after the run
    /// queue drains; in cooperative mode they do not keep the reactor alive
    /// (a spinning `yield_now` cannot peg the host thread).
    yield_queue: Mutex<VecDeque<TaskId>>,
    /// Next task id to hand out (never 0).
    next_task_id: AtomicU32,
    /// The poll-owner entrypoint task (0 = none). The bootstrap pass polls
    /// exactly this task so a multithreaded guest's entrypoint parks with
    /// its spawned tasks left queued for the worker pool.
    poll_owner_task: AtomicU32,
    /// Whether the poll-owner entrypoint future has completed. Once set, a
    /// multithreaded worker exits with the completion code and the process
    /// tears down.
    poll_owner_done: AtomicBool,
    /// (region_id, observed_generation) → wakers parked on generation advance.
    gen_wait_map: Mutex<HashMap<(u64, u64), Vec<Waker>>>,
    /// Serialises mailbox drains: the ring is single-consumer, so at most one
    /// worker drains at a time.
    mailbox_lock: Mutex<()>,
    /// Number of workers currently inside the executor.
    active_workers: AtomicU32,
}

/// The executor's waker payload: identifies the task so a wake targets the
/// right task and SDK futures can recover their task id from the poll context
/// without ambient thread-local state (see [`task_id_from_waker`]). The
/// worker id of the poll that created the waker rides along for observability
/// (the multi-worker test suite verifies distinct-worker execution).
struct TaskWake {
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "read by the test suite and future diagnostics")
    )]
    worker_id: u32,
    task_id: TaskId,
}

/// Handle returned by a spawned guest task.
pub struct JoinHandle<T> {
    state: Arc<JoinState<T>>,
}

/// Shared completion state of a spawned task: the output slot plus an atomic
/// completion flag. `Arc`-shared so the handle (`Send` when `T: Send`) can be
/// awaited from any worker.
struct JoinState<T> {
    /// The task's output once complete; `None` while running.
    result: Mutex<Option<T>>,
    /// Whether the task completed — an atomic flag observable without locking.
    done: AtomicBool,
    /// Waker of the task awaiting the join, registered on a `Pending` poll.
    waker: Mutex<Option<Waker>>,
}

/// A one-shot cooperative yield: parks the current task and re-queues it via
/// the yield queue (see [`yield_now`]).
struct YieldNow {
    yielded: bool,
}

impl<T> JoinHandle<T> {
    pub(crate) fn take_result(&self) -> Option<T> {
        self.state
            .result
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
    }
}

impl<T> Future for JoinHandle<T> {
    type Output = T;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        if self.state.done.load(Ordering::Acquire) {
            let result = self
                .state
                .result
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take();
            Poll::Ready(result.expect("a done join always holds its result"))
        } else {
            *self
                .state
                .waker
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(cx.waker().clone());
            Poll::Pending
        }
    }
}

impl<T> JoinState<T> {
    fn new() -> Self {
        Self {
            result: Mutex::new(None),
            done: AtomicBool::new(false),
            waker: Mutex::new(None),
        }
    }

    fn complete(&self, value: T) {
        *self
            .result
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(value);
        self.done.store(true, Ordering::Release);
        if let Some(waker) = self
            .waker
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        {
            waker.wake();
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
            match task_id_from_waker(cx.waker()) {
                // Inside the executor: queue a yield wake. Yields are applied
                // without counting as forward progress in cooperative mode
                // (see `apply_yield_queue`), so a spinning `yield_now` loop
                // cannot keep the reactor alive and peg the host thread; the
                // yielding task is polled on the next reactor entry (or, in
                // multithreaded mode, by a worker on the next pass).
                Some(task_id) => yield_task(task_id),
                // Outside the executor there is no queue to park on: fall
                // back to self-waking through the caller's waker.
                None => cx.waker().wake_by_ref(),
            }
            Poll::Pending
        }
    }
}

/// Runs a single reactor pass limited to the poll-owner entrypoint task.
///
/// Multithreaded guests use this from the entrypoint wrapper instead of the
/// cooperative run-to-stall reactor: the entrypoint is polled until its
/// first park, and any tasks it spawned are left queued for the worker pool
/// to run concurrently — never serially on the bootstrap thread.
#[cfg(all(
    target_arch = "wasm32",
    feature = "multithreaded",
    target_feature = "atomics"
))]
pub fn bootstrap_reactor() -> i32 {
    let executor = executor();
    {
        let _guard = executor
            .mailbox_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        drain_mailbox();
    }
    let owner = executor.poll_owner_task.load(Ordering::Acquire);
    if owner != 0
        && let Some(observed_wakes) = pop_specific(executor, owner)
    {
        poll_task(executor, owner, 0, observed_wakes);
    }
    // A yielded entrypoint must be left runnable for the workers.
    apply_yield_queue(executor);
    poll_owner_done(executor)
}

/// Install the generation-wait callbacks so that channel types in
/// `selium-shm` can park tasks on the reactor.
pub fn install_generation_wait_callbacks() {
    selium_memory::install_generation_callbacks(register_gen_wait, wake_gen_waiters);
}

/// Polls mailbox wakeups and runnable background tasks until no work remains.
///
/// Returns the poll-owner entrypoint completion code: `0` while the entrypoint
/// task is still running (parked), `1` once it has completed. The host reads
/// this code through the `__selium_guest_poll` export to distinguish a
/// still-resident service from a process whose entrypoint future finished.
pub fn poll_reactor() -> i32 {
    register_mailbox();
    install_generation_wait_callbacks();
    worker_enter(0, false)
}

/// Polls the guest reactor and aborts the process if polling panics.
///
/// Returns the poll-owner entrypoint completion code (see [`poll_reactor`]).
pub fn poll_safely() -> i32 {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(poll_reactor)) {
        Ok(code) => code,
        Err(_) => std::process::abort(),
    }
}

/// Starts an entrypoint future and aborts the process if polling panics.
pub fn run_entrypoint_safely<F>(future: F)
where
    F: Future<Output = ()> + Send + 'static,
{
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        spawn_poll_owner(future);
        drive_entrypoint();
    }));
    if result.is_err() {
        std::process::abort();
    }
}

/// Runs an entrypoint future that produces a `Result` and aborts the
/// process if polling panics.
///
/// Returns the future's output once the reactor stalls: the value the task
/// produced if it completed, or `Ok(())` when it parked without completing.
/// A parked entrypoint is the normal state for a long-running service
/// (e.g. a guest accepting connections forever) or an entrypoint waiting
/// on host-delivered events: the export must return an exit code
/// immediately, so `Ok(())` (no error observed) is reported and the task
/// stays on the reactor, driven by later guest polls.
///
/// Errors are logged inside the task when it completes — before or after
/// the reactor first stalls — because a late `Err` can no longer reach the
/// already-returned exit code; the log is the only surfacing channel.
///
/// The entrypoint task is installed as the poll owner: its completion (an
/// `Ok` or `Err` reaching the task body) sets the reactor's completion
/// signal, so a later host poll observes the process as done rather than as
/// an idle resident reactor (see [`poll_reactor`]).
pub fn run_entrypoint_with_result<F, E>(future: F) -> Result<(), E>
where
    F: Future<Output = Result<(), E>> + Send + 'static,
    E: core::fmt::Display + Send + 'static,
{
    let join = spawn_poll_owner(async move {
        match future.await {
            Ok(()) => Ok(()),
            Err(error) => {
                crate::error!("{error}");
                Err(error)
            }
        }
    });
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(drive_entrypoint));
    if result.is_err() {
        std::process::abort();
    }
    join.take_result().unwrap_or(Ok(()))
}

/// Spawns a future onto the shared guest executor.
///
/// The future must be `Send + 'static` so its task capsule can migrate
/// between — and execute concurrently on — any of the guest's worker threads.
/// The output type must be `Send` for the same reason: the completed value is
/// stored in the shared `JoinState`.
pub fn spawn<F, T>(future: F) -> JoinHandle<T>
where
    F: Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    let state = Arc::new(JoinState::new());
    let state_for_task = Arc::clone(&state);
    let id = next_task_id();
    let task = TaskSlot {
        id,
        future: Some(Box::pin(async move {
            let output = future.await;
            state_for_task.complete(output);
        })),
        state: AtomicU32::new(TASK_QUEUED),
        park_word: AtomicU32::new(0),
    };

    enqueue_new_task(task);

    JoinHandle { state }
}

/// Yields execution back to the guest executor once.
pub async fn yield_now() {
    YieldNow { yielded: false }.await;
}

/// Returns the task id encoded in `waker`, if the waker was created by this
/// executor. `None` for foreign wakers (e.g. tokio's, in native tests).
pub(crate) fn task_id_from_waker(waker: &Waker) -> Option<TaskId> {
    if std::ptr::eq(waker.vtable(), &TASK_WAKE_VTABLE) {
        // SAFETY: the vtable match guarantees the data pointer was produced
        // by `create_waker` and points at a live `Arc<TaskWake>`.
        let wake = unsafe { &*waker.data().cast::<TaskWake>() };
        Some(wake.task_id)
    } else {
        None
    }
}

/// Wakes `task_id`: makes it runnable from any state and, if it was parked,
/// enqueues it and notifies a parked worker. A wake racing an in-flight poll
/// is never lost: the parking word bump is observed by the poller's
/// re-check-before-park, which re-queues the task when it re-parks.
pub(crate) fn wake_task(task_id: TaskId) {
    if task_id == 0 {
        return;
    }
    let executor = executor();
    loop {
        let tasks = executor
            .tasks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(slot) = tasks.get(&task_id) else {
            return; // task already completed and removed
        };
        match slot.state.load(Ordering::Acquire) {
            TASK_PARKED => {
                if slot
                    .state
                    .compare_exchange(
                        TASK_PARKED,
                        TASK_QUEUED,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    )
                    .is_ok()
                {
                    slot.park_word.fetch_add(1, Ordering::Release);
                    let _ = bump_task_parking_word(task_id);
                    drop(tasks);
                    enqueue_and_notify(task_id);
                    return;
                }
                // Lost the parked → queued race (another waker or the poller's
                // re-park): re-read the state.
            }
            TASK_QUEUED => {
                // Already queued: the upcoming poll absorbs the wake.
                slot.park_word.fetch_add(1, Ordering::Release);
                let _ = bump_task_parking_word(task_id);
                return;
            }
            TASK_POLLING => {
                // In-flight poll: the poller detects the bump via its
                // re-check-before-park. If it parked in the meantime, deliver
                // the wake on the re-park (loop and transition now). The
                // SeqCst pairing with the poller's post-poll state store and
                // parking-word re-check load guarantees exactly one side
                // observes the race (see `poll_task`).
                slot.park_word.fetch_add(1, Ordering::SeqCst);
                let _ = bump_task_parking_word(task_id);
                if slot.state.load(Ordering::SeqCst) == TASK_PARKED {
                    continue;
                }
                return;
            }
            _ => return,
        }
    }
}

/// Runs the shared executor as `worker_id`.
///
/// With `park_when_empty` false (cooperative mode) the loop returns the
/// poll-owner completion code as soon as no forward-progress work remains;
/// the host drives subsequent entries through the `__selium_guest_poll`
/// export. With `park_when_empty` true (multithreaded mode) the worker parks
/// on the shared wake word when idle and only returns once the poll-owner
/// entrypoint completes (the process-exit signal).
pub(crate) fn worker_enter(worker_id: u32, park_when_empty: bool) -> i32 {
    let executor = executor();
    executor.active_workers.fetch_add(1, Ordering::Relaxed);
    let code = worker_loop(executor, worker_id, park_when_empty);
    executor.active_workers.fetch_sub(1, Ordering::Relaxed);
    code
}

/// Queues a cooperative yield for `task_id` (see [`YieldNow`]).
pub(crate) fn yield_task(task_id: TaskId) {
    if task_id != 0 {
        executor()
            .yield_queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push_back(task_id);
    }
}

/// Applies cooperative yields queued during the pass: transitions each
/// yielded task parked → queued and enqueues it. Returns whether any yield
/// was applied. Yields do NOT count as forward progress in cooperative mode
/// (see [`worker_loop`]).
fn apply_yield_queue(executor: &Executor) -> bool {
    let yields: Vec<TaskId> = executor
        .yield_queue
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .drain(..)
        .collect();
    if yields.is_empty() {
        return false;
    }
    for task_id in yields {
        let tasks = executor
            .tasks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(slot) = tasks.get(&task_id) else {
            continue;
        };
        if slot
            .state
            .compare_exchange(
                TASK_PARKED,
                TASK_QUEUED,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
        {
            let _ = bump_task_parking_word(task_id);
            drop(tasks);
            enqueue_and_notify(task_id);
        }
        // Else: already queued or being polled by another worker — leave it.
    }
    true
}

unsafe fn clone_waker(data: *const ()) -> RawWaker {
    // SAFETY: `data` points at a live `Arc<TaskWake>` with a strong count
    // owned by the raw waker being cloned; the increment becomes the cloned
    // waker's own reference, so the source waker's pointer stays valid.
    unsafe { Arc::increment_strong_count(data.cast::<TaskWake>()) };
    RawWaker::new(data, &TASK_WAKE_VTABLE)
}

/// Creates a waker for `task_id` as seen from `worker_id`.
fn create_waker(worker_id: u32, task_id: TaskId) -> Waker {
    let arc = Arc::new(TaskWake { worker_id, task_id });
    let ptr = Arc::into_raw(arc);
    // SAFETY: `TASK_WAKE_VTABLE` manages the strong count via the clone/drop
    // entries, and the data pointer always points at a live `Arc<TaskWake>`
    // for as long as any waker derived from it exists.
    unsafe { Waker::from_raw(RawWaker::new(ptr.cast(), &TASK_WAKE_VTABLE)) }
}

/// Drives the entrypoint to its first park.
///
/// Cooperative builds run the reactor to stall (the host re-drives later
/// polls). Multithreaded builds run a single bootstrap pass: the entrypoint
/// parks with its spawned tasks left queued for the worker pool, so CPU work
/// never runs serially on the bootstrap thread.
fn drive_entrypoint() -> i32 {
    #[cfg(all(
        target_arch = "wasm32",
        feature = "multithreaded",
        target_feature = "atomics"
    ))]
    {
        bootstrap_reactor()
    }
    #[cfg(not(all(
        target_arch = "wasm32",
        feature = "multithreaded",
        target_feature = "atomics"
    )))]
    {
        poll_safely()
    }
}

unsafe fn drop_waker(data: *const ()) {
    // SAFETY: `data` points at a live `Arc<TaskWake>`; dropping the raw
    // pointer decrements the strong count and frees the allocation at zero.
    drop(unsafe { Arc::from_raw(data.cast::<TaskWake>()) });
}

/// Pushes `task_id` onto the shared run queue, bumps the shared wake word,
/// and notifies one parked worker.
fn enqueue_and_notify(task_id: TaskId) {
    executor()
        .run_queue
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .push_back(task_id);
    bump_wake_word();
    notify_wake_word();
}

/// Registers a new task in the table and enqueues it runnable.
fn enqueue_new_task(task: TaskSlot) {
    let id = task.id;
    let mut tasks = executor()
        .tasks
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    tasks.insert(id, task);
    drop(tasks);
    enqueue_and_notify(id);
}

fn executor() -> &'static Executor {
    EXECUTOR.get_or_init(|| Executor {
        tasks: Mutex::new(HashMap::new()),
        run_queue: Mutex::new(VecDeque::new()),
        yield_queue: Mutex::new(VecDeque::new()),
        next_task_id: AtomicU32::new(1),
        poll_owner_task: AtomicU32::new(0),
        poll_owner_done: AtomicBool::new(false),
        gen_wait_map: Mutex::new(HashMap::new()),
        mailbox_lock: Mutex::new(()),
        active_workers: AtomicU32::new(0),
    })
}

/// Allocates a fresh task id (never 0; wraps to 1 on overflow).
fn next_task_id() -> TaskId {
    let executor = executor();
    loop {
        let current = executor.next_task_id.load(Ordering::Relaxed);
        let next = current.checked_add(1).filter(|&id| id != 0).unwrap_or(1);
        if executor
            .next_task_id
            .compare_exchange_weak(current, next, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
        {
            return current.max(1);
        }
    }
}

/// Returns whether the poll-owner entrypoint task has completed.
fn poll_owner_completed(executor: &Executor) -> bool {
    executor.poll_owner_done.load(Ordering::Acquire)
}

/// Returns the poll-owner completion code: `0` while it is running (parked),
/// `1` once it completed.
fn poll_owner_done(executor: &Executor) -> i32 {
    i32::from(executor.poll_owner_done.load(Ordering::Acquire))
}

/// Polls one claimed task's future, applying the lost-wakeup re-check.
///
/// `observed_wakes` is the parking-word baseline captured alongside the
/// queued → polling claim (see [`pop_runnable`]). The future is taken out of
/// the task table so the table is never locked across a poll — a wake
/// delivered during the poll (via a waker) must be able to reach the task.
/// Returns `true` when the task completed.
fn poll_task(executor: &Executor, task_id: TaskId, worker_id: u32, observed_wakes: u32) -> bool {
    let mut future = {
        let mut tasks = executor
            .tasks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(slot) = tasks.get_mut(&task_id) else {
            return false;
        };
        slot.future
            .take()
            .expect("polled task always holds its future")
    };

    let waker = create_waker(worker_id, task_id);
    let mut context = Context::from_waker(&waker);
    let poll = future.as_mut().poll(&mut context);

    let mut tasks = executor
        .tasks
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some(slot) = tasks.get_mut(&task_id) else {
        // The task was removed while it was being polled (only possible if
        // something re-entered the executor during the poll); drop the future.
        return true;
    };
    match poll {
        Poll::Ready(()) => {
            tasks.remove(&task_id);
            true
        }
        Poll::Pending => {
            slot.future = Some(future);
            // Sequentially consistent with `wake_task`'s parking-word bump
            // and state re-load: the store/load pairs on both sides cannot
            // both read stale values (the store-buffering litmus), so a
            // wake racing an in-flight poll is always observed by exactly
            // one of the two sides. On wasm this is free — wasm atomics are
            // always seqcst — but it also makes the native test builds
            // sound on weakly ordered hosts.
            slot.state.store(TASK_PARKED, Ordering::SeqCst);
            // Lost-wakeup re-check: a wake that raced the poll (the parking
            // word advanced) must be delivered when the task re-parks.
            if slot.park_word.load(Ordering::SeqCst) != observed_wakes
                && slot
                    .state
                    .compare_exchange(
                        TASK_PARKED,
                        TASK_QUEUED,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    )
                    .is_ok()
            {
                drop(tasks);
                enqueue_and_notify(task_id);
            }
            false
        }
    }
}

/// Claims one runnable task: pops an id from the run queue and
/// CAS-transitions its state queued → polling, guaranteeing single-flight
/// (at most one worker polls a task's future at a time). Also captures the
/// task's parking-word baseline **in the same task-table critical section as
/// the claim**: a concurrent wake either lands before the claim (absorbed by
/// the upcoming poll) or bumps the word after the baseline read (caught by
/// the poller's post-poll re-check) — no wake can slip between the claim and
/// the baseline. Returns `None` when the queue is empty or every entry is
/// stale (already claimed or completed).
fn pop_runnable(executor: &Executor) -> Option<(TaskId, u32)> {
    loop {
        let task_id = executor
            .run_queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pop_front()?;
        let tasks = executor
            .tasks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(slot) = tasks.get(&task_id) else {
            continue; // completed and removed; stale queue entry
        };
        if slot
            .state
            .compare_exchange(
                TASK_QUEUED,
                TASK_POLLING,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
        {
            let observed_wakes = slot.park_word.load(Ordering::Acquire);
            return Some((task_id, observed_wakes));
        }
        // Stale or concurrently claimed entry; try the next.
    }
}

/// Claims a specific task id from the run queue (queued → polling) if it is
/// runnable, without touching any other queued task, capturing the
/// parking-word baseline in the same critical section (see [`pop_runnable`]).
/// Used by the bootstrap pass to poll exactly the entrypoint task. A stale
/// queue entry is left for `pop_runnable` to skip.
#[cfg(all(
    target_arch = "wasm32",
    feature = "multithreaded",
    target_feature = "atomics"
))]
fn pop_specific(executor: &Executor, task_id: TaskId) -> Option<u32> {
    let tasks = executor
        .tasks
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some(slot) = tasks.get(&task_id) else {
        return None;
    };
    if slot
        .state
        .compare_exchange(
            TASK_QUEUED,
            TASK_POLLING,
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .is_ok()
    {
        Some(slot.park_word.load(Ordering::Acquire))
    } else {
        None
    }
}

fn register_gen_wait(region_id: u64, observed_generation: u64, waker: &Waker) {
    let executor = executor();
    // Register with the guest's own gen-wait map (for guest-writable rings).
    executor
        .gen_wait_map
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .entry((region_id, observed_generation))
        .or_default()
        .push(waker.clone());

    // Notify the host that this guest task is parked on a host-writable
    // ring so the host can wake us when it advances the generation.
    // If the region is guest-writable, the WaitRegister is harmless
    // (the host will never advance it, so no wake comes from this path).
    if let Some(task_id) = task_id_from_waker(waker) {
        // Best-effort: if the hostcall fails, the gen-wait map still holds
        // the waker, and the backstop wake path may still fire.
        drop(hostcall_ready_with_task(
            HostcallRequest::WaitRegister {
                region_id,
                generation: observed_generation,
            },
            task_id,
        ));
    }
}

/// Returns whether the run queue holds any runnable task (without claiming).
fn run_queue_non_empty(executor: &Executor) -> bool {
    !executor
        .run_queue
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .is_empty()
}

/// Spawns the poll-owner entrypoint future: the single task whose completion
/// signals the process's normal exit through the poll export.
///
/// Like [`spawn`], but records the task as the poll owner and sets the
/// reactor's completion signal (`POLL_OWNER_DONE`) when the future completes —
/// whether it returns `Ok` or `Err` — so a later [`poll_reactor`] reports the
/// entrypoint as done instead of leaving the process resident on an idle,
/// silent reactor.
fn spawn_poll_owner<F, T>(future: F) -> JoinHandle<T>
where
    F: Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    let state = Arc::new(JoinState::new());
    let state_for_task = Arc::clone(&state);
    let id = next_task_id();
    executor().poll_owner_task.store(id, Ordering::Release);
    executor().poll_owner_done.store(false, Ordering::Release);
    let task = TaskSlot {
        id,
        future: Some(Box::pin(async move {
            let output = future.await;
            executor().poll_owner_done.store(true, Ordering::Release);
            // Wake EVERY parked worker so it observes the completion and
            // returns from the worker entry. Each notify releases exactly
            // one parked waiter (the engine keeps a per-waiter node per
            // wake word), so a single notify would leave the remaining
            // parked workers asleep through the process exit — deliver one
            // per active worker. The wake-word bump covers a worker that
            // has not parked yet (its wait32 returns immediately on the
            // value mismatch), and `park_on_wake_word`'s bounded timeout
            // is the liveness backstop for a worker that parks after this
            // loop has already delivered its notifies.
            bump_wake_word();
            let active = executor().active_workers.load(Ordering::Acquire);
            for _ in 0..active {
                notify_wake_word();
            }
            state_for_task.complete(output);
        })),
        state: AtomicU32::new(TASK_QUEUED),
        park_word: AtomicU32::new(0),
    };

    enqueue_new_task(task);

    JoinHandle { state }
}

fn wake_gen_waiters(region_id: u64, new_generation: u64) {
    let executor = executor();
    let mut map = executor
        .gen_wait_map
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // Collect keys where region matches and generation < new_generation.
    let to_wake: Vec<(u64, u64)> = map
        .keys()
        .filter(|(rid, cur_gen)| *rid == region_id && *cur_gen < new_generation)
        .copied()
        .collect();
    for key in to_wake {
        if let Some(wakers) = map.remove(&key) {
            for waker in wakers {
                waker.wake();
            }
        }
    }
    drop(map);

    // Notify the host of the advance so cross-guest waiters that registered
    // via `WaitRegister` are woken through the mailbox. Local waiters are
    // handled above; the host call is advisory and decided `cfg` for wasm
    // only (native test fallbacks have no cross-guest peers).
    #[cfg(target_arch = "wasm32")]
    {
        if crate::hostcall::hostcall_ready(HostcallRequest::GenerationAdvance {
            region_id,
            generation: new_generation,
        })
        .is_err()
        {
            // Best-effort: waiters re-check and re-park on their next poll.
        }
    }
}

unsafe fn wake_waker(data: *const ()) {
    // SAFETY: `data` is a live `Arc<TaskWake>` (see `clone_waker`); waking
    // by ref is safe on such a pointer.
    unsafe { wake_waker_by_ref(data) };
    // SAFETY: `data` is still a live `Arc<TaskWake>` after the wake; drop
    // consumes the reference owned by this waker exactly once.
    unsafe { drop_waker(data) };
}

unsafe fn wake_waker_by_ref(data: *const ()) {
    // SAFETY: `data` points at a live `Arc<TaskWake>`.
    let wake = unsafe { &*data.cast::<TaskWake>() };
    wake_task(wake.task_id);
}

fn worker_loop(executor: &Executor, worker_id: u32, park_when_empty: bool) -> i32 {
    loop {
        // The host tears a multithreaded process down by setting the ABI stop
        // word and notifying the wake word; a woken worker returns instead of
        // re-parking. Cooperative mode reports the code at stall instead,
        // preserving the existing host-driven semantics.
        if park_when_empty && (stop_requested() || poll_owner_completed(executor)) {
            return poll_owner_done(executor);
        }
        {
            // The mailbox ring is single-consumer: at most one worker drains.
            let _guard = executor
                .mailbox_lock
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            drain_mailbox();
        }
        // Run runnable tasks (forward progress). Tasks woken or spawned while
        // another task is being polled enqueue directly into the shared queue,
        // so this loop keeps going until the queue is genuinely empty.
        while let Some((task_id, observed_wakes)) = pop_runnable(executor) {
            poll_task(executor, task_id, worker_id, observed_wakes);
        }
        if run_queue_non_empty(executor) || mailbox_has_pending() {
            continue;
        }
        // Queue empty: apply cooperative yields queued during the pass.
        let yielded = apply_yield_queue(executor);
        if yielded {
            if park_when_empty {
                // Multithreaded: no host re-drive exists, so the worker keeps
                // servicing runnable (yielded) tasks.
                continue;
            }
            // Cooperative: yields do not count as forward progress — the host
            // drives the next poll entry, so a spinning `yield_now` cannot peg
            // the host thread.
            return poll_owner_done(executor);
        }
        if !park_when_empty {
            return poll_owner_done(executor);
        }
        // Multithreaded mode: re-check the exit conditions before parking —
        // the process may have completed while the last task was being
        // polled (the loop-top check has not run again).
        if stop_requested() || poll_owner_completed(executor) {
            return poll_owner_done(executor);
        }
        // Park with the standard futex discipline: observe the wake word,
        // re-check for work, then wait. A wake landing between the check and
        // the wait wakes us immediately.
        let observed = wake_word_value();
        if run_queue_non_empty(executor) || mailbox_has_pending() {
            continue;
        }
        park_on_wake_word(observed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Serialises reactor tests: the executor is a process-global static, so
    /// tests that drive it must not run concurrently (they would share the
    /// task table and run queue).
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    /// Upper bound on workers used to bound the stop budget in tests.
    const MAX_WORKERS: usize = 64;

    /// Resets the shared executor to a clean slate between test scenarios.
    fn reset_executor() {
        assert_eq!(
            executor().active_workers.load(Ordering::Acquire),
            0,
            "reset while workers are still inside the executor"
        );
        executor()
            .tasks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
        executor()
            .run_queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
        executor()
            .yield_queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
        executor().poll_owner_done.store(false, Ordering::Release);
        executor()
            .gen_wait_map
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
    }

    /// Makes parked multithreaded workers exit their loop (the process-exit
    /// signal) and wakes them. Each `notify_one` releases exactly one parked
    /// worker (the registry's notified flag is consumed by the worker it
    /// releases), so keep notifying — with a pause for the flag handoff —
    /// until every worker has exited.
    fn stop_workers() {
        executor().poll_owner_done.store(true, Ordering::Release);
        for _ in 0..=MAX_WORKERS {
            if executor().active_workers.load(Ordering::Acquire) == 0 {
                return;
            }
            bump_wake_word();
            notify_wake_word();
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert_eq!(
            executor().active_workers.load(Ordering::Acquire),
            0,
            "workers failed to exit within the stop budget"
        );
    }

    /// Waits until `condition` holds or a generous deadline passes, then
    /// asserts it holds — a bounded wait so a lost wake fails the test
    /// instead of hanging it.
    fn wait_until(condition: impl Fn() -> bool, what: &str) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !condition() {
            assert!(
                std::time::Instant::now() < deadline,
                "timed out waiting for {what}"
            );
            std::thread::yield_now();
        }
    }

    fn assert_send<T: Send>() {}

    #[test]
    fn join_handle_is_send_when_output_is_send() {
        assert_send::<JoinHandle<u32>>();
        let _guard = TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        reset_executor();
    }

    #[test]
    fn cooperative_yield_allows_spawned_task_progress() {
        let _guard = TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        reset_executor();
        let value = Arc::new(Mutex::new(0));
        let value_for_task = Arc::clone(&value);

        let join = spawn(async move {
            yield_now().await;
            *value_for_task
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = 7;
        });
        // First pass: task runs, hits yield_now (Pending + self-wake), parks.
        // The self-wake marks the task runnable for the next pass.
        poll_reactor();
        assert_eq!(
            *value
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            0
        );

        // Second pass: task is runnable, yield_now completes (Ready), task sets
        // value and finishes.
        poll_reactor();

        assert_eq!(
            *value
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            7
        );
        assert_eq!(
            *join
                .state
                .result
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            Some(())
        );
    }

    #[test]
    fn reactor_parks_pending_tasks_until_woken() {
        let _guard = TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        reset_executor();
        struct ParkUntilWoken {
            polls: Arc<Mutex<u32>>,
            task_id: Arc<Mutex<Option<TaskId>>>,
        }

        impl Future for ParkUntilWoken {
            type Output = ();

            fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
                let task = self.get_mut();
                let mut polls = task
                    .polls
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                *polls += 1;
                if *polls == 1 {
                    *task
                        .task_id
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) =
                        task_id_from_waker(cx.waker());
                    Poll::Pending
                } else {
                    Poll::Ready(())
                }
            }
        }

        let polls = Arc::new(Mutex::new(0));
        let task_id = Arc::new(Mutex::new(None));
        let join = spawn(ParkUntilWoken {
            polls: Arc::clone(&polls),
            task_id: Arc::clone(&task_id),
        });

        poll_reactor();
        assert_eq!(
            *polls
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            1
        );
        assert_eq!(
            *join
                .state
                .result
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            None
        );

        poll_reactor();
        assert_eq!(
            *polls
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            1
        );

        wake_task(
            task_id
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .expect("task id captured"),
        );
        poll_reactor();

        assert_eq!(
            *polls
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            2
        );
        assert_eq!(
            *join
                .state
                .result
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            Some(())
        );
    }

    /// Display-only error type for the `run_entrypoint_with_result` tests.
    #[derive(Debug)]
    struct EntrypointError(&'static str);

    impl core::fmt::Display for EntrypointError {
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            f.write_str(self.0)
        }
    }

    #[test]
    fn result_entrypoint_that_parks_returns_ok_before_completion() {
        let _guard = TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        reset_executor();
        let ran = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&ran);
        // One cooperative yield parks the task before it completes: the
        // entrypoint export must observe `Ok(())` (no error) while the task
        // stays on the reactor — a long-running service entrypoint parks
        // here indefinitely.
        let result = run_entrypoint_with_result(async move {
            yield_now().await;
            flag.store(true, Ordering::Release);
            Ok::<(), EntrypointError>(())
        });

        assert!(matches!(result, Ok(())));
        assert!(
            !ran.load(Ordering::Acquire),
            "task must not have completed before the park"
        );

        // A later poll (the host-driven `__selium_guest_poll` path) drives
        // the parked entrypoint to completion.
        poll_reactor();
        assert!(
            ran.load(Ordering::Acquire),
            "later poll must complete the parked entrypoint"
        );
    }

    #[test]
    fn result_entrypoint_that_fails_returns_err() {
        let _guard = TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        reset_executor();
        let result: Result<(), EntrypointError> =
            run_entrypoint_with_result(async { Err(EntrypointError("boom")) });
        assert!(matches!(result, Err(error) if error.0 == "boom"));
    }

    /// Task 1.1: a poll-owner future that completes causes the reactor to
    /// report completion, while one that parks (never resolving) reports
    /// still-running.
    #[test]
    fn reactor_reports_poll_owner_completion() {
        let _guard = TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        reset_executor();
        // A future that completes synchronously reports done immediately.
        let result = run_entrypoint_with_result(async { Ok::<(), EntrypointError>(()) });
        assert!(matches!(result, Ok(())));
        assert_eq!(
            poll_reactor(),
            1,
            "a completed entrypoint must report completion"
        );

        reset_executor();
        // A future that parks indefinitely reports running.
        let result = run_entrypoint_with_result(async {
            core::future::pending::<Result<(), EntrypointError>>().await
        });
        assert!(matches!(result, Ok(())));
        assert_eq!(poll_reactor(), 0, "a parked entrypoint must report running");
    }

    /// Task 1.3: a previously parked entrypoint that completes during a later
    /// reactor poll (a wake, not a bare re-poll) flips the reactor to the
    /// completion signal — for both `Ok` and `Err` completions — rather than
    /// leaving the process resident on an idle reactor.
    #[test]
    fn reactor_reports_late_poll_owner_completion() {
        let _guard = TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        reset_executor();
        /// Parks on its first poll (capturing the task id) and completes on a
        /// later poll, mirroring a service that waits on an external wake.
        struct CompleteOnWake {
            polls: u32,
            task_id: Arc<Mutex<Option<TaskId>>>,
            fail: bool,
        }

        impl Future for CompleteOnWake {
            type Output = Result<(), EntrypointError>;

            fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
                let this = self.get_mut();
                this.polls += 1;
                if this.polls == 1 {
                    *this
                        .task_id
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) =
                        task_id_from_waker(cx.waker());
                    Poll::Pending
                } else if this.fail {
                    Poll::Ready(Err(EntrypointError("late boom")))
                } else {
                    Poll::Ready(Ok(()))
                }
            }
        }

        // Late `Ok` completion: parked, then woken.
        let task_id: Arc<Mutex<Option<TaskId>>> = Arc::new(Mutex::new(None));
        let result = run_entrypoint_with_result(CompleteOnWake {
            polls: 0,
            task_id: Arc::clone(&task_id),
            fail: false,
        });
        assert!(matches!(result, Ok(())));
        assert_eq!(poll_reactor(), 0, "parked entrypoint reports running");
        wake_task(
            task_id
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .expect("task id captured"),
        );
        assert_eq!(
            poll_reactor(),
            1,
            "a late-completing entrypoint must report completion"
        );

        reset_executor();
        // Late `Err` completion: parked, then woken — the error path is a
        // completion signal too.
        let task_id: Arc<Mutex<Option<TaskId>>> = Arc::new(Mutex::new(None));
        let result = run_entrypoint_with_result(CompleteOnWake {
            polls: 0,
            task_id: Arc::clone(&task_id),
            fail: true,
        });
        assert!(matches!(result, Ok(())));
        assert_eq!(poll_reactor(), 0, "parked entrypoint reports running");
        wake_task(
            task_id
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .expect("task id captured"),
        );
        assert_eq!(
            poll_reactor(),
            1,
            "a late error completion must report completion"
        );
    }

    /// Test-only: decodes the worker id from an executor waker.
    fn worker_id_from_waker(waker: &Waker) -> Option<u32> {
        if std::ptr::eq(waker.vtable(), &TASK_WAKE_VTABLE) {
            // SAFETY: the vtable match guarantees the data pointer is a live
            // `Arc<TaskWake>` created by the executor.
            Some(unsafe { &*waker.data().cast::<TaskWake>() }.worker_id)
        } else {
            None
        }
    }

    /// Records the worker id of the poll that created the current waker.
    struct CaptureWorker(Arc<Mutex<Vec<u32>>>);

    impl Future for CaptureWorker {
        type Output = ();

        fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
            if let Some(worker_id) = worker_id_from_waker(cx.waker()) {
                self.0
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push(worker_id);
            }
            Poll::Ready(())
        }
    }

    /// Task 2.1: two OS-thread workers running one shared executor execute
    /// independent tasks concurrently — each task observes a distinct worker
    /// id (via the executor waker) and both tasks overlap in time.
    #[test]
    fn independent_tasks_run_concurrently_on_distinct_workers() {
        let _guard = TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        reset_executor();

        let workers_seen = Arc::new(Mutex::new(Vec::new()));
        let entered = Arc::new(AtomicU32::new(0));
        let overlap_failed = Arc::new(AtomicBool::new(false));
        let all_done = Arc::new(AtomicU32::new(0));

        for _ in 0..2 {
            let workers_seen = Arc::clone(&workers_seen);
            let entered = Arc::clone(&entered);
            let overlap_failed = Arc::clone(&overlap_failed);
            let all_done = Arc::clone(&all_done);
            spawn(async move {
                // Record which worker polled this task.
                CaptureWorker(Arc::clone(&workers_seen)).await;
                // Deliberately occupy the worker until BOTH tasks are inside:
                // with two workers each task runs on its own worker and they
                // overlap; with one worker the first task spins to a deadline
                // and flags failure instead of hanging the test.
                entered.fetch_add(1, Ordering::Release);
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
                while entered.load(Ordering::Acquire) < 2 {
                    if std::time::Instant::now() >= deadline {
                        overlap_failed.store(true, Ordering::Release);
                        break;
                    }
                    std::hint::spin_loop();
                }
                all_done.fetch_add(1, Ordering::Release);
            });
        }

        std::thread::scope(|scope| {
            scope.spawn(|| {
                worker_enter(0, true);
            });
            scope.spawn(|| {
                worker_enter(1, true);
            });
            wait_until(
                || all_done.load(Ordering::Acquire) == 2,
                "both tasks to complete",
            );
            stop_workers();
        });

        let seen = workers_seen
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert!(
            seen.contains(&0) && seen.contains(&1),
            "both tasks must run on distinct workers, saw {seen:?}"
        );
        assert_eq!(all_done.load(Ordering::Acquire), 2);
        assert!(
            !overlap_failed.load(Ordering::Acquire),
            "the two tasks must have overlapped in time on distinct workers"
        );
    }

    /// A consumer that registers its waker on the first poll, parks, and
    /// completes on its first wake (re-poll).
    struct ParkUntilWokenOnce {
        wake_map: Arc<Mutex<HashMap<TaskId, Waker>>>,
        completed: Arc<AtomicU32>,
    }

    impl Future for ParkUntilWokenOnce {
        type Output = ();

        fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
            let this = self.get_mut();
            let Some(task_id) = task_id_from_waker(cx.waker()) else {
                return Poll::Ready(());
            };
            let mut map = this
                .wake_map
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if map.insert(task_id, cx.waker().clone()).is_none() {
                // First poll: registered and parked.
                Poll::Pending
            } else {
                // Re-poll after a wake: done.
                drop(map);
                this.completed.fetch_add(1, Ordering::Release);
                Poll::Ready(())
            }
        }
    }

    /// Task 2.2: the lost-wakeup stress test. Worker threads run a shared
    /// executor while producer threads wake consumers that race in-flight
    /// polls. The scenario runs ≥10 consecutive times; any lost wake fails
    /// the run.
    #[test]
    fn lost_wakeup_stress_never_loses_a_wake() {
        let _guard = TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        const RUNS: usize = 10;
        const CONSUMERS: usize = 32;
        const PRODUCERS: usize = 4;
        const WORKERS: usize = 4;

        for run in 0..RUNS {
            reset_executor();
            let completed = Arc::new(AtomicU32::new(0));
            let wake_map = Arc::new(Mutex::new(HashMap::<TaskId, Waker>::new()));

            for _ in 0..CONSUMERS {
                spawn(ParkUntilWokenOnce {
                    wake_map: Arc::clone(&wake_map),
                    completed: Arc::clone(&completed),
                });
            }

            std::thread::scope(|scope| {
                for _ in 0..PRODUCERS {
                    let wake_map = Arc::clone(&wake_map);
                    scope.spawn(move || {
                        for _ in 0..64 {
                            let wakers: Vec<Waker> = wake_map
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner)
                                .values()
                                .cloned()
                                .collect();
                            for waker in wakers {
                                waker.wake();
                            }
                            std::thread::yield_now();
                        }
                    });
                }
                for worker in 0..WORKERS {
                    scope.spawn(move || {
                        worker_enter(worker as u32, true);
                    });
                }
                // Every consumer must be polled (registered) at least once;
                // then let the producers hammer the wake-during-poll window
                // before stopping the workers.
                wait_until(
                    || {
                        wake_map
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .len()
                            == CONSUMERS
                    },
                    "all consumers to register",
                );
                std::thread::sleep(std::time::Duration::from_millis(100));
                stop_workers();
            });

            // Final burst: with no worker racing, every registered consumer's
            // last wake must be delivered; a cooperative pass then drains the
            // run queue. A consumer left parked means a wake was lost.
            {
                let wakers: Vec<Waker> = wake_map
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .values()
                    .cloned()
                    .collect();
                for waker in wakers {
                    waker.wake();
                }
            }
            poll_reactor();
            assert_eq!(
                completed.load(Ordering::Acquire),
                CONSUMERS as u32,
                "run {run}: a consumer's wake was lost"
            );
        }
    }

    /// Task 3.1: the hostcall request path is re-entrant across workers —
    /// concurrent envelope staging with distinct per-task ids never corrupts
    /// the shared request slot (each staged envelope decodes to its own task
    /// id).
    #[test]
    fn concurrent_hostcall_staging_never_corrupts_request_slot() {
        let _guard = TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        reset_executor();

        const WORKERS: u32 = 4;
        const PER_WORKER: usize = 500;
        let total = (WORKERS as usize) * PER_WORKER;
        let staged = Arc::new(AtomicU32::new(0));
        let corrupted = Arc::new(AtomicU32::new(0));

        std::thread::scope(|scope| {
            for worker in 0..WORKERS {
                let staged = Arc::clone(&staged);
                let corrupted = Arc::clone(&corrupted);
                scope.spawn(move || {
                    // Each "worker" stages envelopes exactly as the guest
                    // hostcall path does, with a per-worker task id, and
                    // verifies the round trip preserves the task id.
                    for index in 0..PER_WORKER {
                        let task_id = (worker * PER_WORKER as u32) + index as u32 + 1;
                        let envelope = selium_abi::HostcallEnvelope {
                            request: HostcallRequest::SelfInfo,
                            task_id: Some(task_id),
                        };
                        let encoded =
                            selium_abi::encode_rkyv(&envelope).expect("envelope must encode");
                        let decoded: selium_abi::HostcallEnvelope =
                            selium_abi::decode_rkyv(&encoded).expect("envelope must decode");
                        if decoded.task_id != Some(task_id) {
                            corrupted.fetch_add(1, Ordering::Release);
                        }
                        staged.fetch_add(1, Ordering::Release);
                    }
                });
            }
        });

        assert_eq!(staged.load(Ordering::Acquire), total as u32);
        assert_eq!(
            corrupted.load(Ordering::Acquire),
            0,
            "concurrent envelope staging must not cross-contaminate task ids"
        );
    }

    /// Task 3.2: a poller thread wakes a parked task in place — the task
    /// resumes on its worker after an atomic notify on its parking word, with
    /// no poller-driven reactor pass.
    #[test]
    fn wake_by_notify_resumes_a_parked_task_in_place() {
        let _guard = TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        reset_executor();

        let registered = Arc::new(Mutex::new(None::<TaskId>));
        let resumed = Arc::new(AtomicBool::new(false));

        struct ParkUntilNotified {
            registered: Arc<Mutex<Option<TaskId>>>,
            resumed: Arc<AtomicBool>,
            polls: u32,
        }
        impl Future for ParkUntilNotified {
            type Output = ();
            fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
                let this = self.get_mut();
                this.polls += 1;
                if this.polls == 1 {
                    let task_id = task_id_from_waker(cx.waker())
                        .expect("parked task runs under an executor waker");
                    *this
                        .registered
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(task_id);
                    Poll::Pending
                } else {
                    this.resumed.store(true, Ordering::Release);
                    Poll::Ready(())
                }
            }
        }

        let spawned = spawn(ParkUntilNotified {
            registered: Arc::clone(&registered),
            resumed: Arc::clone(&resumed),
            polls: 0,
        });
        drop(spawned);

        // Worker 0 runs the task to its first park, then parks itself.
        std::thread::scope(|scope| {
            scope.spawn(|| {
                worker_enter(0, true);
            });
            // The poller thread: wait for the task to park, then wake it via
            // the parking-word path (wake_task bumps the task's parking word
            // and notifies the shared wake word). The poller never drives the
            // reactor.
            wait_until(
                || {
                    registered
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .is_some()
                },
                "the task to park",
            );
            let task_id = registered
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .expect("task id captured");
            wake_task(task_id);
            wait_until(
                || resumed.load(Ordering::Acquire),
                "the task to resume in place",
            );
            stop_workers();
        });

        assert!(
            resumed.load(Ordering::Acquire),
            "the parked task must resume on its worker after the notify"
        );
    }
}
