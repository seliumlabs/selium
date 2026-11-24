//! The runtime's timer driver for guest-issued `Sleep` hostcalls.
//!
//! A guest task that awaits [`selium_abi::HostcallRequest::Sleep`] parks on a
//! deadline the host owns, so the host must wake it when that deadline passes
//! — whether or not anything else happens in the platform. The driver is the
//! owner of those deadlines.
//!
//! It deliberately runs on its own thread with its own clock rather than
//! borrowing an ambient executor. Borrowing one is what made the wake
//! unreliable: a guest executed inline on some *other* thread — a kernel
//! poller thread, or this driver's own thread while it was re-entering a
//! guest — is inside somebody else's executor context, and a handle captured
//! from it goes away with that executor. Spawning on a dead handle drops the
//! wake silently, and a dropped wake is indistinguishable from a bug to the
//! guest: the timer simply never fires.

use std::{
    cmp::Reverse,
    collections::BinaryHeap,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use parking_lot::{Condvar, Mutex};
use selium_abi::{OperationId, ProcessId, TaskId};

use crate::runtime::{Runtime, WeakRuntime};

/// One outstanding guest sleep, ordered by deadline.
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct SleepRequest {
    pub(crate) deadline: Instant,
    pub(crate) operation_id: OperationId,
    pub(crate) process_id: ProcessId,
    pub(crate) task_id: TaskId,
}

/// Shared deadline queue: a min-heap of pending sleeps plus the condvar that
/// lets the driver thread sleep until the earliest deadline changes.
#[derive(Default)]
struct DeadlineQueue {
    pending: Mutex<BinaryHeap<Reverse<SleepRequest>>>,
    arrived: Condvar,
    stopped: AtomicBool,
}

/// The runtime's timer driver: one thread owning every outstanding guest
/// sleep deadline.
///
/// Started lazily on the first guest-issued `Sleep`, so a runtime that hosts
/// no timers never pays for the thread. Runtime shutdown signals and joins it
/// within a bounded budget; its non-owning runtime handle does not prevent the
/// final embedder handle from initiating that shutdown.
pub(crate) struct TimerDriver {
    queue: Arc<DeadlineQueue>,
    handle: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl DeadlineQueue {
    /// Pushes a sleep and rouses the driver (which may be parked on an
    /// unbounded wait because the queue was empty).
    fn push(&self, request: SleepRequest) {
        self.pending.lock().push(Reverse(request));
        self.arrived.notify_all();
    }

    /// Parks until the earliest deadline is due (or the queue changes),
    /// returning every sleep that is due on wake. Returns `None` once the
    /// driver is shut down.
    fn take_due(&self) -> Option<Vec<SleepRequest>> {
        let mut pending = self.pending.lock();
        loop {
            if self.stopped.load(Ordering::Acquire) {
                return None;
            }
            let wait = match pending.peek() {
                None => None,
                Some(Reverse(earliest)) => earliest
                    .deadline
                    .checked_duration_since(Instant::now())
                    .or(Some(Duration::ZERO)),
            };
            match wait {
                // An empty queue has nothing to time: wait for a push.
                None => self.arrived.wait(&mut pending),
                Some(remaining) => {
                    if self.arrived.wait_for(&mut pending, remaining).timed_out() {
                        break;
                    }
                }
            }
        }

        let now = Instant::now();
        let mut due = Vec::new();
        while let Some(Reverse(earliest)) = pending.peek() {
            if earliest.deadline > now {
                break;
            }
            if let Some(Reverse(request)) = pending.pop() {
                due.push(request);
            }
        }
        Some(due)
    }

    fn shutdown(&self) {
        self.stopped.store(true, Ordering::Release);
        self.arrived.notify_all();
    }
}

impl TimerDriver {
    /// Returns the driver for `runtime`, starting its thread on first use.
    pub(crate) fn for_runtime(runtime: &Runtime) -> &TimerDriver {
        runtime.timer_driver.get_or_init(|| {
            let queue = Arc::new(DeadlineQueue::default());
            let spawned = std::thread::Builder::new()
                .name("selium-timer-driver".to_string())
                .spawn({
                    let queue = Arc::clone(&queue);
                    let runtime = runtime.background_handle();
                    move || drive(queue, runtime)
                });
            TimerDriver {
                queue,
                // A spawn failure degrades to "guest timers never fire" — the
                // same observable as a dead driver — rather than taking the
                // host down over a background thread.
                handle: Mutex::new(spawned.ok()),
            }
        })
    }

    /// Stops the driver thread and joins it. Idempotent.
    pub(crate) fn shutdown(&self) {
        self.queue.shutdown();
        if let Some(handle) = self.handle.lock().take() {
            crate::runtime::join_bounded(handle);
        }
        // The driver no longer completes deadlines: release what it retained.
        self.queue.pending.lock().clear();
    }

    /// Registers a guest sleep, to be completed at `deadline`.
    pub(crate) fn schedule(&self, request: SleepRequest) {
        self.queue.push(request);
    }

    /// Drops every sleep queued by `process_id`.
    ///
    /// A stopped process will never consume the completion wake, so its
    /// deadlines must not linger in the driver — a guest parked on a long
    /// `Timer` would otherwise leave a queue entry behind for its full
    /// duration (the soak lane's timer finding).
    pub(crate) fn cancel_process(&self, process_id: ProcessId) {
        self.retain(|request| request.process_id != process_id);
    }

    /// Drops a single cancelled sleep so a dropped timer operation releases
    /// its deadline immediately instead of waiting for process teardown.
    pub(crate) fn cancel_operation(&self, operation_id: OperationId) {
        self.retain(|request| request.operation_id != operation_id);
    }

    fn retain(&self, keep: impl Fn(&SleepRequest) -> bool) {
        let mut pending = self.queue.pending.lock();
        let before = pending.len();
        pending.retain(|Reverse(request)| keep(request));
        if pending.len() != before {
            // The earliest deadline may have moved; wake the driver so it
            // re-parks against the new head.
            self.queue.arrived.notify_all();
        }
    }

    /// Returns the number of guest sleeps still queued on the driver.
    ///
    /// The soak lane reads this to prove a reaped process does not strand its
    /// deadlines in the driver.
    pub(crate) fn pending_count(&self) -> usize {
        self.queue.pending.lock().len()
    }
}

impl Runtime {
    /// Marks a guest sleep the driver declared due as complete and delivers the
    /// wake.
    ///
    /// The wake goes out through the ordinary mailbox path, so the guest's
    /// parked task re-polls and observes `Ready`. A sleep whose operation the
    /// guest already dropped (it cancelled the timer, or the task was dropped)
    /// is *not* woken: there is nothing left to re-poll, and an unconditional
    /// wake would keep a cancelled timer churning the reactor.
    pub(crate) fn complete_sleep(&self, request: &SleepRequest) {
        let completed = {
            let mut operations = self.operations.lock();
            match operations.get_mut(&request.operation_id) {
                Some(operation) => {
                    operation.state = crate::hostcall::HostOperationState::Ready(
                        selium_abi::HostcallOutput::Empty,
                    );
                    true
                }
                None => false,
            }
        };
        if completed {
            // Cross-thread wakes are safe here: see the memory-model contract
            // on `poll_guest_until_stalled`.
            self.wake_process_task(request.process_id, request.task_id);
        }
    }
}

/// The driver loop: park until the earliest deadline, then complete every
/// sleep that came due.
fn drive(queue: Arc<DeadlineQueue>, runtime: WeakRuntime) {
    while let Some(due) = queue.take_due() {
        for request in due {
            let Some(runtime) = runtime.upgrade() else {
                return;
            };
            runtime.complete_sleep(&request);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;

    /// The queue hands a sleep back exactly once its deadline passes, and
    /// never hands back a sleep that is not yet due.
    #[test]
    fn due_sleeps_are_handed_back_at_their_deadline() {
        let queue = DeadlineQueue::default();
        queue.push(SleepRequest {
            deadline: Instant::now() + Duration::from_millis(20),
            operation_id: 1,
            process_id: 7,
            task_id: 1,
        });

        let start = Instant::now();
        let due = queue.take_due().expect("driver is running");
        assert_eq!(due.len(), 1, "the queued sleep must come back");
        assert!(start.elapsed() >= Duration::from_millis(15));
        assert_eq!(due.first().map(|request| request.operation_id), Some(1));
    }

    /// A sleep pushed while the driver is parked on an unbounded wait (empty
    /// queue) still arrives — the push must rouse the condvar.
    #[test]
    fn push_wakes_an_idle_driver() {
        let queue = Arc::new(DeadlineQueue::default());
        let driver = {
            let queue = Arc::clone(&queue);
            std::thread::spawn(move || queue.take_due())
        };
        // Give the driver a moment to park on the empty queue.
        std::thread::sleep(Duration::from_millis(20));
        queue.push(SleepRequest {
            deadline: Instant::now(),
            operation_id: 2,
            process_id: 9,
            task_id: 1,
        });

        let due = driver
            .join()
            .expect("driver thread")
            .expect("driver is running");
        assert_eq!(due.len(), 1);
        assert_eq!(due.first().map(|request| request.operation_id), Some(2));
    }
}
